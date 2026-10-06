//! The Settings → Folders sub-page (`ui/folders.md`) — the control-plane
//! core: folder list + in-place config (paths / conflict
//! policy), the conflict review list, the create wizard, and the device-local
//! folder↔set binding section nested under each expanded row (A6 Slice 3 — the
//! `folder-location-*` family, driven through `crate::sync_agent`; see
//! [`location_elements`]).
//!
//! A paint shell over the SAME shared `DevicesMachine` (`libs/fauna-devices-machine`)
//! the Devices roster (`super::devices`) already builds at session attach — this
//! page reads its `folders` / `conflicts` / `wizard` snapshot fields instead of
//! `devices`, and drives the embedded `FolderWizardMachine` the machine already
//! owns (`fauna_devices_machine::DevicesMachine::wizard`). No second machine
//! instance, no FFI hop (priority #2) — consumed directly like linux's
//! `views/devices_folders/`.
//!
//! The page-level `sync-default-conflict-policy-select` lives here too, and is
//! NOT a `DevicesMachine` gesture: it seals into `fauna.state.sync-prefs` through
//! the **direct preference-surface** idiom this crate already uses for muted words /
//! backups / task delegation — no FFI face is involved (tui consumes the shared
//! crates directly, so an FFI hop would be the priority-#2 violation, not the
//! route). The wizard stamps the loaded value onto newly created sets only.
//!
//! The two per-set **author** gestures live here as well — `folder-webdav-toggle`
//! (owner rows) and `folder-paywall-tier-select` (website-enabled rows), the
//! structural siblings. Both drive shared `FoldersAuthor` orchestrations over the
//! conversations rail's ONE per-actor `MlsEngine`, and both are gated on a
//! capability read **per page-visit off the key-bearing config** — never the
//! keyless `DevicesMachine` snapshot, which cannot answer either question.
//!
//! **Cross-user sharing** (`ui/folders.md` § Sharing a folder) landed
//! 2026-07-30 and lives here too, in the two halves the goal doc names:
//!
//! - **Owner side**, in the expanded row's body — the "Shared with" section
//!   (`folder-share-button` over the REUSED `recipient-picker-input`, the
//!   `folder-member-item` roster with per-member access + byte cap, and the
//!   `folder-shared-badge` "Shared · N"), all over `FoldersAuthor::share_set` /
//!   `remove_member` and `FoldersClient::members_set_access` — crate-direct over
//!   the conversations rail's ONE per-actor `MlsEngine`, like the two author
//!   gestures above.
//! - **Recipient side**, page-level and on the row header — the "Shared with you"
//!   knock list (`folder-pending-share` + accept/decline) over
//!   `fauna_client_inbox::list_folder_pending_shares`, and on a `role ==
//!   "member"` row the "Shared by ‹who›" badge + `folder-leave-button`.
//!
//! ⚠ The recipient half also needs the B3 join-filter wired
//! (`super::devices::wire_mls_query`) — un-wired, `DevicesMachine` drops every
//! member row and nothing painted here can appear.

use fauna_devices_machine::{ConflictSummary, FolderSummary};
use fauna_folders_machine::{
    FolderWizardSnapshot, FolderWizardStep, SubmitPhase, conflict_badge_label,
    conflict_policy_options, join_paths_field,
};
use fauna_i18n::strings::common;
use fauna_i18n::strings::devices as t;
use fauna_ui_ids as ids;
// NOT aliased `fs` — the per-set loops below bind `fs` to a `FolderSummary`.
use fauna_i18n::strings::folders as fs_strings;
use fauna_i18n::strings::settings::sync_page as sp;

use super::{Action, PlaceFlag, SettingsField, SettingsState};
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::sync_agent::RenderedLocationBinding;

// The `folder-nest-snapshots-select` wire values, its option catalog, and the
// buffers⇄policy rules all live in shared Rust
// (`fauna_folders_machine::nest_place`) so the seven apps painting this editor
// share one implementation of "blank is a value" and of retention's inverted
// omission rule. tui was the lead app and held the only copy until the six
// apps' slice-e trickle-down lifted it out.
use fauna_folders_machine::{
    nest_place_edit_from_row, nest_snapshots_label, nest_snapshots_options,
};
// The save gesture is assembled in `super` (the action → `Op` mapping), so this
// one is re-exported rather than merely used here.
pub(super) use fauna_folders_machine::nest_place_write;

/// This sub-page's local UI-only state (never part of the machine, never
/// persisted): which row is expanded, its path-edit buffers, and the armed
/// destructive-delete confirm. Reset on every fresh nav into the page
/// ([`super::route_subpage`]) — mirrors the Privacy/Account sub-pages' own
/// reveal-state resets.
#[derive(Debug, Clone, Default)]
pub struct FoldersUiState {
    /// The index of the one `folder-row` whose body (paths + save + delete) is
    /// open, if any — only one at a time is addressable (the linux
    /// `AdwExpanderRow` shape every other app's e2e helper already assumes:
    /// `find_and_expand_folder` toggles exactly one row and the unindexed body
    /// widgets then resolve uniquely to it).
    pub expanded: Option<usize>,
    /// The expanded row's `folder-include-paths` buffer, pre-filled from
    /// `FolderSummary::include_paths` on expand; committed on
    /// `folder-save-paths`, never written live per keystroke (unlike the
    /// wizard's fields, this is a real network call).
    pub include_paths_input: String,
    /// Same shape as [`Self::include_paths_input`] for `exclude_paths`.
    pub exclude_paths_input: String,
    /// The expanded row's staged `folder-nest-snapshots-select` value — one of
    /// `fauna_folders_machine::NEST_SNAPSHOTS_{DEFAULT,ON,OFF}`. Staged rather than
    /// applied-on-change (unlike `folder-conflict-policy-select`) because the nest
    /// place's policy is sent WHOLE: an apply-on-change select would have to
    /// send the two retention boxes and the quiet period with it, committing
    /// half-typed values the user had not saved.
    pub nest_snapshots_input: String,
    /// The expanded row's `folder-nest-quiet-input` buffer — seconds of quiet
    /// before the nest place cuts a snapshot. **Blank means unset** (the
    /// nest-wide cadence), which is a real third state, not a missing value
    /// (`backup-restore.md` § 8b). Pre-filled on expand, committed on
    /// `folder-nest-save-button` with its two retention siblings.
    pub nest_quiet_input: String,
    /// The expanded row's `folder-nest-retention-snapshots` buffer — max
    /// snapshot count. Blank = that bound unset; blank in BOTH retention boxes
    /// clears the policy to keep-everything. A blank never means "keep zero"
    /// (the nest reads a zero bound as unset — `backup/retention.rs`
    /// `parse_folder_retention`).
    pub nest_retention_snapshots_input: String,
    /// Same shape as [`Self::nest_retention_snapshots_input`] for max age days.
    pub nest_retention_days_input: String,
    /// The version-retention pair (`folder-version-retention-{count,days}`) —
    /// the sibling policy family that bounds file-version history
    /// (`file-versions.md` § Retention ruling 1). Same blank = unset rules,
    /// prefills via `version_retention_edit_from_bounds` (shared Rust).
    pub version_retention_count_input: String,
    /// Same shape as [`Self::version_retention_count_input`] for max age days.
    pub version_retention_days_input: String,
    /// What [`prefill_nest_place`] last wrote into the six nest-place buffers,
    /// or `None` when the buffers are not the user's (nothing seeded yet, or
    /// [`release_nest_place`] handed them back at the save). A refresh re-seeds
    /// only while the buffers still equal this — see
    /// [`reseed_nest_place_if_untouched`].
    pub nest_place_seeded: Option<[String; 6]>,
    /// Whether `folder-delete-button`'s destructive confirm
    /// (`folder-delete-confirm`) is armed for the expanded row.
    pub delete_pending: bool,
    /// Whether the follow flow (`folder-follow-*`) is open. Page-level, not
    /// per-row: a follow names someone else's folder, so it belongs to the page
    /// rather than to any row already in the list — the `share_open` shape, one
    /// level up.
    pub follow_open: bool,
    /// The owner's handle in the follow flow, typed into the REUSED
    /// `recipient-picker-input` (no new picker IDs — priority #2, as the share
    /// flow does).
    pub follow_handle_input: String,
    /// The followed folder's name, plaintext. Public folders' names are
    /// world-readable by the audience contract, which is exactly why this is
    /// typed in the clear rather than resolved from a sealed listing.
    pub follow_name_input: String,
    /// Whether the DECLASSIFY confirm (`folder-audience-public-confirm`) is
    /// armed for the expanded row — picking `Public` on `folder-audience-select`
    /// arms it, exactly as `folder-delete-button` arms [`Self::delete_pending`].
    ///
    /// The flip to `public` is the one audience transition that carries an
    /// explicit owner confirm, and it is a product invariant rather than a
    /// courtesy: a public folder rests UNSEALED, names and paths included
    /// (`principles.md` § The user always controls their data owns that single
    /// exception to sealed-at-rest). So the select **must not** commit on change
    /// the way every other control on this row does.
    ///
    /// ⚠ While this is armed the select keeps painting the folder's CURRENT
    /// audience, never the pending one. The nest has not moved yet, and a
    /// control that showed `Public` before the confirm would be reporting an
    /// audience the folder does not have — the same "a refused serve must not
    /// leave the toggle looking on" rule `Op::ServeSetFolder` states.
    pub audience_public_pending: bool,
    /// Whether the content-residency confirm (`folder-residency-confirm`) is
    /// armed for the expanded row — picking `Metadata only` on
    /// `folder-nest-residency-select` arms it (phase 5, `file-sync.md`
    /// § Content residency). The same shape as [`Self::audience_public_pending`]
    /// for the same reason: the flip deletes the nest's copy of the folder's
    /// content, and v1 has no custody-inferred softening — the owner's explicit
    /// consent is the only gate. While armed the select keeps painting the
    /// folder's CURRENT residency, never the pending one.
    pub residency_pending: bool,
    /// The `folder-location-path-input` typed-path buffer for the expanded row's
    /// folder↔set binding add form (Slice 3). One buffer suffices — only the one
    /// expanded row renders the section — mirroring `include_paths_input`. Bound
    /// on `folder-location-add-button` to the expanded row's set (contextual — no
    /// free-text set name), then cleared; tui carries no `folder-location-browse-button`
    /// (an OS-picker affordance — `tui.md` § Declared platform absences 4).
    pub location_path_input: String,
    /// The page-level `sync-default-conflict-policy-select` reading —
    /// `fauna.state.sync-prefs`'s `default_conflict_policy`, loaded on the page's one
    /// awaited nav-edge op and re-read after each save. `None` means *no
    /// preference recorded* (or pre-login), which the select renders as `auto` —
    /// the equivalent outcome for a new set (`ui/folders.md` § Element IDs).
    ///
    /// This is deliberately NOT a `DevicesMachine` field: it is one sealed
    /// `fauna.state.sync-prefs` preference, so it rides the direct preference-surface idiom this
    /// crate already uses for muted words / backups / task delegation ("no
    /// machine earns its keep for a one-field seam" — linux's own
    /// `build_sync_defaults_section`). The wizard stamps it onto NEW sets only;
    /// existing rows keep their own `folder-conflict-policy-select` value.
    pub default_conflict_policy: Option<String>,
    /// Whether this actor may serve a set over WebDAV — the shared
    /// `owner_can_serve_webdav` (i.e. "holds an MSEK"), read on the page's one
    /// awaited nav-edge op.
    ///
    /// ⚠ Read **per page-visit off the key-bearing config**, never carried on the
    /// `DevicesMachine` snapshot, which is keyless and cannot answer it
    /// (`ui/folders.md:59`). `false` gates `folder-webdav-toggle` DISABLED with
    /// a "set up mail first" hint: serving seals the `WebdavKeysBlob` under the
    /// MSEK, and `serve_set` commits the nest flag *before* it re-provisions, so
    /// an MSEK-less click would leave the set served-but-blobless. Disabling makes
    /// that state unreachable rather than merely reported.
    pub can_serve_webdav: bool,
    /// The creator's own subscription tier names (`SubscriptionsClient::tiers_list`,
    /// ascending by rank) — the `folder-paywall-tier-select` option set on
    /// website-enabled rows. Read per page-visit like [`Self::can_serve_webdav`], and
    /// fail-safe empty: no tiers ⇒ the select renders DISABLED with a "create a
    /// tier first" hint, since there is nothing to paywall to
    /// (`ui/folders.md:60`).
    pub own_tiers: Vec<String>,
    /// The owner-side "Shared with" roster for the ONE expanded row —
    /// `fauna.folders.members.list_actors` as returned, **unfiltered**. The
    /// `role == "member"` filter is applied at render through the single shared
    /// derivation site `fauna_client_folders::member_actors`, which backs BOTH
    /// the `folder-member-item` list and the `folder-shared-badge` count;
    /// never re-filter `role` locally (`ui/folders.md:175`).
    ///
    /// One roster, not a per-row map, for the same reason
    /// [`Self::include_paths_input`] is one buffer: only one row is expanded at a
    /// time. [`Self::roster_for`] names the set it belongs to, so a stale roster
    /// from a previously expanded row can never be painted under a different set.
    pub members: Vec<fauna_protocol::folders::FolderActorMember>,
    /// The expanded row's DEVICE roster (`fauna.folders.members.list`) — what
    /// the places editor (`folder-place-row`) renders. Read on expand beside
    /// the actor roster; guarded by [`Self::device_places_for`] exactly as the
    /// roster is by `roster_for`.
    pub device_places: Vec<fauna_protocol::folders::FolderMember>,
    /// The set [`Self::device_places`] was read FOR — the fresh guard.
    pub device_places_for: Option<String>,
    /// The expanded row's DESTINATION places (`backup-destinations.md`
    /// § Ordinary-folder coverage): every enrolled backup destination, marked
    /// attached-or-not for this folder. Read on expand beside the rosters;
    /// guarded by [`Self::destination_places_for`] exactly as the others are.
    pub destination_places: Vec<FolderDestinationPlace>,
    /// The set [`Self::destination_places`] was read FOR — the fresh guard.
    pub destination_places_for: Option<String>,
    /// The attach select's chosen `destination_id` — a plain buffer, exactly
    /// like the paths inputs; cleared when the row collapses or the coverage
    /// repaints.
    pub destination_attach_selection: String,
    /// The set name [`Self::members`] was read for — the guard against painting
    /// one row's roster under another.
    pub roster_for: Option<String>,
    /// The expanded row's derived `ChannelId` (hex), echoed from the roster read
    /// — what `FoldersAuthor::remove_member` addresses the set by. `None` for an
    /// unshared set (no group ⇒ no members ⇒ nothing to remove).
    pub roster_channel_id: Option<String>,
    /// The expanded row's device-activity rows — `fauna.folders.devices`, the
    /// ordinary sync "who has recorded a change, and how many" signal
    /// (distinct from `FolderSummary::cached_snapshot_count`/`cached_total_bytes`,
    /// which are snapshot-only). Read on the SAME awaited round trip as
    /// [`Self::members`] when a row expands (`Op::LoadFolderRoster`), and
    /// RE-read on every `fauna.sync.changed` push naming this set
    /// (`crate::settings::device_activity_resync_op` → `Op::LoadFolderDeviceActivity`)
    /// — that live update with no manual reload is the whole point of the
    /// render (`file-sync.md` § Implementation status today; it is what makes
    /// web's remote-change nudge e2e-pinnable, and this is tui's twin).
    ///
    /// One list, not a per-row map, for the same reason [`Self::members`] is —
    /// only one row is expanded at a time. [`Self::device_activity_for`] names
    /// the set it belongs to, mirroring [`Self::roster_for`]'s stale-paint
    /// guard: a read for a previously expanded (or since-collapsed) row must
    /// never paint under a different set.
    pub device_activity: Vec<fauna_protocol::folders::FolderDevice>,
    /// The set name [`Self::device_activity`] was read for — mirrors
    /// [`Self::roster_for`].
    pub device_activity_for: Option<String>,
    /// Whether the share form is open on the expanded row — the
    /// `folder-share-button` gesture arms it, exactly like [`Self::delete_pending`]
    /// arms `folder-delete-confirm`. tui has no modal dialogs, so linux's
    /// `adw::MessageDialog` becomes inline elements gated on this flag.
    pub share_open: bool,
    /// The `recipient-picker-input` buffer for the open share form. The picker
    /// is REUSED by id only (`ui/folders.md:147` — "no new picker IDs"): linux
    /// likewise instantiates the whole `RecipientPicker` widget and then consumes
    /// nothing but `picker.input.text()`, so the chip/suggestion/resolve
    /// machinery is outside the share contract on every app.
    pub share_recipient_input: String,
    /// The `folder-share-role-select` value for the open share form — the
    /// share-time access grant, `"reader"` by default (`ui/folders.md:152`).
    pub share_access: String,
    /// The `folder-member-cap-input` buffers, positionally aligned with the
    /// RENDERED (member-filtered) roster. Rebuilt from the nest's authoritative
    /// `byte_cap` on every roster read, so an edit never survives a refresh that
    /// contradicts it.
    pub member_cap_inputs: Vec<String>,
    /// The recipient-side staged shares — un-acked `channel_type == "folder"`
    /// welcomes from a STRANGER (a contact's share auto-joins off the chat rail
    /// and never lands here). Read per page-visit on the one awaited nav-edge op,
    /// like the three capability reads above: the section is fetched on
    /// page-visible, not pushed.
    pub pending_shares: Vec<PendingShareView>,
    /// The offline ceremony's two halves of the same page surface: consent
    /// cards awaiting an answer, and the shared sets whose machinery landed
    /// (`p2p.md` § Offline share initiation). Read on the SAME awaited
    /// nav-edge op as `pending_shares` above and for the same reason — both
    /// knock lists are fetched on page-visible rather than pushed, so the
    /// shared helper's Devices→Folders toggle re-fires both at once.
    #[cfg(feature = "p2p-share")]
    pub group_shares: crate::offline_share::GroupShareViews,
    /// The `sync-agent-linger-toggle` reading, resolved once on page entry
    /// (`crate::sync_agent::linger_enabled`) and re-resolved after each flip —
    /// never per frame, since it costs a `loginctl` subprocess. `None` means the
    /// machine has no lingering concept to offer, and the row is not rendered at
    /// all (`sync-agent.md` § Headless deployment).
    pub linger_enabled: Option<bool>,
}

// ── The page-level default-conflict-policy seam (`sync-default-conflict-policy-select`) ──
//
// Thin error-bridging over the shared
// `fauna_sync_engine::preference_surfaces::{load,save}_sync_prefs`, over the
// account store (waited for when the page is opened before the runtime is
// up); the one normalizer is `preference_records::set_default_conflict_policy`
// and this module holds no logic of its own. The async side takes only `Send`
// inputs (the handle source), so no `&App` crosses the spawn boundary.

/// Read `fauna.state.sync-prefs`'s `default_conflict_policy`. Runs on the Folders
/// page's one awaited nav-edge op; `None` = no preference recorded.
///
/// The read is answered from the replica's own account store
/// (`account-client-lifecycle.md` § The client-side lifecycle: "reads answered
/// from the handle").
pub(super) async fn load_default_conflict_policy(
    store: &impl fauna_sync_engine::account_runtime::AccountStoreAccess,
) -> Result<Option<String>, String> {
    fauna_sync_engine::preference_surfaces::load_sync_prefs(store)
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
}

/// Persist the page-level default and hand back what was actually stored after
/// normalization — the render source.
///
/// The save is the account store's `put_preference`.
pub(super) async fn save_default_conflict_policy(
    store: &impl fauna_sync_engine::account_runtime::AccountStoreAccess,
    policy: String,
) -> Result<Option<String>, String> {
    fauna_sync_engine::preference_surfaces::save_sync_prefs(store, Some(&policy))
        .await
        .map_err(fauna_sync_engine::preference_surfaces::plane_failure)
}

// ── The two per-set author gestures (`folder-webdav-toggle` / `-paywall-tier-select`) ──
//
// tui's instantiation of the owner-side shared-folder author, over the shared
// `fauna_client_folders::build_folders_author` recipe every native app (and
// `fauna-ffi`) now calls — the WS-RPC `FoldersClient`, the owner's account-plane
// content-key custody, and — the load-bearing part — the conversations rail's
// ONE live per-actor `MlsEngine` as the group-crypto seam. Never a second
// engine: two would race the single `mls_state.db` (`ui/folders.md` § Sharing
// → *Where logic lives*).

type FoldersAuthorNative = fauna_client_folders::orchestration::FoldersAuthor<
    std::sync::Arc<fauna_client::NestClient>,
    std::sync::Arc<fauna_mls::engine::MlsEngine>,
>;

/// Unix seconds, for reading a lease's `expires_at` — the only clock the
/// status line consults (`folder_lease_status` takes it as a parameter so the
/// shared rule stays clock-free).
fn lease_clock_now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64)
}

fn build_folders_author(
    nest: std::sync::Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
    session: &fauna_conversations::ConversationsSession,
) -> FoldersAuthorNative {
    fauna_client_folders::build_folders_author(
        nest,
        fauna_core::identity::ActorKeypair::from_secret(secret),
        folder_keys,
        mail,
        session,
    )
}

/// Decode the hex MLS **group id** a shared set carries into its derived
/// `ChannelId` — the shared `fauna_mls::types::ChannelId::from_group_id_hex`
/// (not a fixed-32 decode): a group id is not 32 bytes, and `from_group_id`
/// hashes whatever length it is — the exact bug linux's own regression test
/// pins.
pub(super) fn channel_id_from_group_id_hex(
    mls_group_id_hex: &str,
) -> Result<fauna_mls::types::ChannelId, hex::FromHexError> {
    fauna_mls::types::ChannelId::from_group_id_hex(mls_group_id_hex)
}

fn channel_id_of(mls_group_id: Option<String>) -> Result<Option<[u8; 32]>, String> {
    match mls_group_id {
        Some(hex) => Ok(Some(
            channel_id_from_group_id_hex(&hex)
                .map_err(|e| format!("mls_group_id: {e}"))?
                .0,
        )),
        None => Ok(None),
    }
}

/// Whether this actor may serve a set over WebDAV — the shared
/// `owner_can_serve_webdav` ("the account's mail custody holds an MSEK"), read
/// through the session's account-store handle. No handle yet, or a failed
/// read, answers `false`, the fail-safe direction: withholding the control is
/// recoverable, offering one whose flip commits a nest flag and then fails is
/// not (the page re-reads on its next hydrate).
pub(super) async fn load_can_serve_webdav(
    store: Option<fauna_sync_engine::account_runtime::AccountStoreHandle>,
) -> bool {
    let Some(handle) = store else {
        return false;
    };
    fauna_client_folders::webdav_provision::owner_can_serve_webdav(&handle)
        .await
        .unwrap_or(false)
}

/// The creator's own subscription tier names, ascending by rank — the
/// `folder-paywall-tier-select` option set. Fail-safe empty (a failed read
/// renders the select disabled with the "create a tier first" hint rather than
/// offering tiers that may not exist).
pub(super) async fn load_own_tiers(nest: std::sync::Arc<fauna_client::NestClient>) -> Vec<String> {
    fauna_client_subscriptions::SubscriptionsClient::new(nest)
        .tiers_list()
        .await
        .map(|tiers| tiers.into_iter().map(|t| t.name).collect())
        .unwrap_or_default()
}

/// Set a folder's audience — the phase-4 keyless `folders.update` setter.
///
/// **Keyless for every direction, including the bound `→shared` flip-back.** All three destinations the picker can send — `public`,
/// `private`, and `shared` on a bound folder exiting its public window —
/// converge off the projected audience itself
/// (`SyncEngine::converge_corpus_to_audience`, which dispatches the sealed
/// direction on bound-ness), on every seat, the members' included. No custody
/// sentinel is staged for any of them: the sentinel is per-actor, so it could
/// never reach a member's engine, and phase 4 ruled the projection IS the
/// cross-device signal.
///
/// Consequently this needs no `ConversationsSession` and no `FoldersAuthor`,
/// unlike `serve_set` and `paywall_set` below — nothing here touches a content
/// key. What it does carry is the owner's **identity** key: a `→public` flip
/// is the owner confirm's landing point, so `FoldersClient::set_audience`
/// mints the owner's signed attestation there (`encryption-at-rest.md`
/// § Readable classes → *The declassification is owner-ATTESTED*), and a seat
/// unseals only on that signature, never on the nest's report. Unwired, the
/// flip would land and every verifying seat would keep the folder sealed.
pub(super) async fn set_audience(
    nest: std::sync::Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    name: String,
    audience: String,
) -> Result<(), String> {
    fauna_client_folders::FoldersClient::new(nest)
        .with_audience_attestor(std::sync::Arc::new(
            fauna_core::identity::ActorKeypair::from_secret(secret),
        ))
        .set_audience(&name, &audience)
        .await
        .map_err(|e| e.to_string())
}

/// Set a folder's content residency — the phase-5 keyless `folders.update`
/// setter (`FoldersClient::set_residency`). `"metadata_only"` reaches here only
/// from the answered `folder-residency-confirm`; `"full"` (the flip back)
/// commits on change. The nest's refusal text — "turn off website serving
/// first" and kin — is the actionable half of any error, so it travels whole.
pub(super) async fn set_residency(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
    residency: String,
) -> Result<(), String> {
    fauna_client_folders::FoldersClient::new(nest)
        .set_residency(&name, &residency)
        .await
        .map_err(|e| e.to_string())
}

/// Turn a folder's exclusive editing on or off — the keyless
/// `FoldersClient::set_exclusive_editing` (`file-sync.md` § Exclusive
/// editing). The nest's refusal text travels whole, as for residency.
pub(super) async fn set_exclusive_editing(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
    on: bool,
) -> Result<(), String> {
    fauna_client_folders::FoldersClient::new(nest)
        .set_exclusive_editing(&name, on)
        .await
        .map_err(|e| e.to_string())
}

/// Flip a folder's website serving (`FoldersAuthor::set_website_enabled`).
///
/// The door to a website folder: phase 2 slice e retired the wizard's mode step,
/// so between then and this slice there was no way to create one at all
/// (`folders.md` § Implementation status today records that accepted gap).
/// Keyless for the same reason as [`set_audience`] — a plain `folders.update`.
pub(super) async fn set_website(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
    enable: bool,
) -> Result<(), String> {
    fauna_client_folders::FoldersClient::new(nest)
        .set_website_enabled(&name, enable)
        .await
        .map_err(|e| e.to_string())
}

/// Flip a folder's WebDAV serving (`FoldersAuthor::serve_set` — the
/// serve-enable/disable + `WebdavKeysBlob` reconcile in one `mls`-gated
/// composition). Answers how many served sets the reconciled blob now carries.
/// An enable also re-seals the set's pre-serve files onto the served key (the
/// flipping client's walk, `webdav-server.md` § Key model (c)), recorded under
/// the device the Media page records under.
#[allow(clippy::too_many_arguments)]
pub(super) async fn serve_set(
    nest: std::sync::Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
    session: std::sync::Arc<fauna_conversations::ConversationsSession>,
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    name: String,
    mls_group_id: Option<String>,
    enable: bool,
    predecessors: Vec<[u8; 32]>,
) -> Result<usize, String> {
    let mut author = build_folders_author(
        std::sync::Arc::clone(&nest),
        secret,
        std::sync::Arc::clone(&folder_keys),
        mail,
        &session,
    );
    if let Some(device_id) = crate::media::device_id_hex_for_secret(secret) {
        author = author.with_served_set_converge(fauna_client_folders::served_set_converge(
            nest,
            &fauna_core::identity::ActorKeypair::from_secret(secret),
            folder_keys,
            device_id,
            predecessors,
        ));
    }
    let channel_id = channel_id_of(mls_group_id)?;
    author
        .serve_set(&name, channel_id, enable)
        .await
        .map_err(|e| e.to_string())
}

/// Create a set through the shared create helper (`FoldersAuthor::create_set` —
/// the set's nonce minted into custody before the nest sees the set).
pub(super) async fn create_set(
    nest: std::sync::Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
    session: std::sync::Arc<fauna_conversations::ConversationsSession>,
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    req: fauna_client_folders::folders::FolderCreateRequest,
) -> Result<(), String> {
    let name = req.name.clone();
    build_folders_author(
        nest,
        secret,
        std::sync::Arc::clone(&folder_keys),
        mail,
        &session,
    )
    .create_set(req)
    .await
    .map(|_| ())
    .map_err(|e| format!("create set {name:?}: {e}"))
}

/// Paywall a website-enabled folder to `tier` (`FoldersAuthor::paywall_set` — content-key
/// genesis/re-seal + the nest `web_paywall_tier` flag + the web-serve-holder
/// `content.read{folder:set}` grant mint). The holder is discovered through
/// `fauna.bridges.fetch_bridge_pubkey` (role `content-processor`, id `web-serve`;
/// the nest self-enrolls it at boot), mirroring the FFI face's
/// `discover_web_serve_holder` — same discovery, crate-direct rather than over
/// UniFFI (priority #2).
#[allow(clippy::too_many_arguments)]
pub(super) async fn paywall_set(
    nest: std::sync::Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
    session: std::sync::Arc<fauna_conversations::ConversationsSession>,
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    grant_log: std::sync::Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    name: String,
    mls_group_id: Option<String>,
    tier: String,
) -> Result<(), String> {
    let author = build_folders_author(
        std::sync::Arc::clone(&nest),
        secret,
        std::sync::Arc::clone(&folder_keys),
        mail,
        &session,
    )
    .with_grant_log(grant_log);
    let channel_id = channel_id_of(mls_group_id)?;
    let holder = fauna_client_bridges::MailAdminClient::new(nest)
        .fetch_bridge_pubkey("content-processor", "web-serve")
        .await
        .map_err(|e| e.to_string())?;
    let holder_pubkey: [u8; 32] = holder.x25519_pubkey.as_slice().try_into().map_err(|_| {
        format!(
            "web-serve holder returned a malformed X25519 pubkey ({} bytes, want 32)",
            holder.x25519_pubkey.len()
        )
    })?;
    let holder_mlkem_ek = holder.mlkem_ek.map(|ek| ek.to_vec());
    author
        .paywall_set(&name, &tier, channel_id, holder_pubkey, holder_mlkem_ek)
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

// ── Cross-user sharing (`ui/folders.md` § Sharing a folder) ──────────────
//
// All eight seams below are the crate-direct twins of linux's `client.rs`
// helpers (`do_share_folder`, `do_remove_folder_member`,
// `read_folder_actors`, `do_list_pending_shares`, `do_accept_share`,
// `do_decline_share`, `do_leave_folder_share`) — same shared crates, same
// ordering guarantees, no FFI hop (priority #2). Where linux reaches a
// process-global `active_session()`, tui takes the session `Arc` as a parameter,
// so nothing here depends on ambient state.

/// One staged, not-yet-accepted cross-user share the recipient renders as a
/// `folder-pending-share`. The tui display projection of
/// [`fauna_client_inbox::FolderPendingShare`] — the raw `welcome_bytes` blob
/// never round-trips through the UI state (accept re-resolves it by `inbox_id`),
/// the linux `PendingShareView` shape.
#[derive(Debug, Clone)]
pub struct PendingShareView {
    /// The durable-inbox row id — the accept/decline target.
    pub inbox_id: i64,
    /// The pre-computed "Shared by ‹…›" label from the shared crate
    /// (`account_display_label`: handle when present, else the canonical
    /// `short_id`). Empty only for a fully unstamped cross-nest share.
    pub shared_by_display: String,
}

/// Peek the recipient's staged folder shares. A pure peek — it never acks.
/// Fail-safe empty: this rides the page's background nav-edge hydrate, where a
/// failed read must not blank the whole page (the `load_own_tiers` posture).
pub(super) async fn load_pending_shares(
    nest: std::sync::Arc<fauna_client::NestClient>,
) -> Vec<PendingShareView> {
    let inbox = fauna_client_inbox::InboxClient::new(nest);
    fauna_client_inbox::list_folder_pending_shares(
        &inbox,
        fauna_client_inbox::PENDING_SHARE_PEEK_LIMIT,
    )
    .await
    .map(|shares| {
        shares
            .into_iter()
            .map(|s| PendingShareView {
                inbox_id: s.inbox_id,
                shared_by_display: s.shared_by_display,
            })
            .collect()
    })
    .unwrap_or_default()
}

/// The owner-side roster for `name` plus the set's derived `ChannelId` (hex).
///
/// `fauna.folders.not_shared` is an EMPTY ROSTER, not a failure — an
/// owner-only set legitimately has no members, and surfacing it as a page error
/// is exactly what `test_folder_owner_side_sharing_affordances` forbids
/// ("that empty state is NOT surfaced as a page error").
pub(super) async fn load_roster(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
    mls_group_id: Option<String>,
) -> Result<
    (
        Vec<fauna_protocol::folders::FolderActorMember>,
        Option<String>,
    ),
    String,
> {
    let channel_id = mls_group_id
        .as_deref()
        .and_then(|hex| channel_id_from_group_id_hex(hex).ok())
        .map(|c| c.to_string());
    let files = fauna_client_folders::FoldersClient::new(nest);
    match files.actor_members_list(name).await {
        Ok(reply) => Ok((reply.members, channel_id)),
        Err(fauna_client::NestClientError::Rpc(ref e)) if e.code == "fauna.folders.not_shared" => {
            Ok((Vec::new(), channel_id))
        }
        Err(e) => Err(e.to_string()),
    }
}

/// The expanded row's DEVICE roster — `fauna.folders.members.list`, the rows
/// the places editor renders. `not_found` is an EMPTY list, not a failure: a
/// member's own bound set can never hold a row in the single-owner roster
/// (`folder_members` requires the device and the set to share one actor), and
/// the editor simply does not render there.
pub(super) async fn load_device_places(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
) -> Result<Vec<fauna_protocol::folders::FolderMember>, String> {
    let folders = fauna_client_folders::FoldersClient::new(nest);
    match folders.members_list(name).await {
        Ok(reply) => Ok(reply.members),
        Err(fauna_client::NestClientError::Rpc(ref e)) if e.code == "fauna.folders.not_found" => {
            Ok(Vec::new())
        }
        Err(e) => Err(e.to_string()),
    }
}

/// One enrolled backup destination, marked attached-or-not for the expanded
/// folder — what the destination-places section renders
/// (`backup-destinations.md` § Ordinary-folder coverage). Lifted into
/// `fauna_client_config` 2026-08-20 when linux became the second consumer of
/// this same list+config-names join (priority #2) — re-exported here so call
/// sites in this module are untouched.
pub(crate) use fauna_client_config::FolderDestinationPlace;

/// The expanded row's destination places — the shared join of the nest's
/// coverage list with this box's `fauna.state.backup` display names. The box is
/// the one this connection is bound to; unprovable ⇒ an error, never a guess.
pub(super) async fn load_folder_destinations(
    nest: std::sync::Arc<fauna_client::NestClient>,
    store: &dyn fauna_client_config::BackupStateStore,
    folder_id: i64,
) -> Result<Vec<FolderDestinationPlace>, String> {
    let source_nest = bound_source_nest(&nest).await?;
    fauna_client_config::list_folder_destinations(nest, store, source_nest, folder_id)
        .await
        .map_err(|e| e.to_string())
}

/// The 32-byte id this connection is bound to — the source-box key of the
/// per-box destination list every coverage read and write names
/// (`fauna_client_pair::resolve_this_nest_id`).
pub(super) async fn bound_source_nest(
    nest: &std::sync::Arc<fauna_client::NestClient>,
) -> Result<[u8; 32], String> {
    fauna_client_pair::resolve_this_nest_id(nest)
        .await?
        .try_into()
        .map_err(|_| "this nest's id was not 32 bytes".to_string())
}

/// One place edit — `fauna.folders.places.set` with the seat's full flag
/// triple (the point applies whole; phase 2 slice f made every point
/// writable). The free-fn-over-`FoldersClient` shape of `set_audience` above.
pub(super) async fn set_place(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
    device_id: String,
    flags: fauna_protocol::folders::PlaceFlags,
) -> Result<(), String> {
    let folders = fauna_client_folders::FoldersClient::new(nest);
    folders
        .places_set(fauna_protocol::folders::PlacesSetRequest {
            name,
            device_id,
            flags,
            ..Default::default()
        })
        .await
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// The expanded row's device-activity rows — `fauna.folders.devices`, a pure
/// replay-safe read (`FoldersClient::devices`). Rides the SAME awaited round
/// trip as [`load_roster`] on expand (`Op::LoadFolderRoster`), and its own
/// narrower round trip on a `fauna.sync.changed` push
/// (`crate::settings::device_activity_resync_op` → `Op::LoadFolderDeviceActivity`)
/// — a push has nothing to do with the "Shared with" roster, so it must never
/// re-fetch (or blank) it.
pub(super) async fn load_device_activity(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
) -> Result<Vec<fauna_protocol::folders::FolderDevice>, String> {
    fauna_client_folders::FoldersClient::new(nest)
        .devices(name)
        .await
        .map(|reply| reply.devices)
        .map_err(|e| e.to_string())
}

/// Follow a public folder by (owner, folder name) — `resolve_public_folder`
/// against the home nest's public plane, then `save_follow` into the account's
/// own `fauna.state.follows` row for the folder.
///
/// **The owner is accepted in either shape**, exactly as [`share_set`] takes a
/// recipient: a bare 64-hex actor id is classified locally, anything else
/// resolves as a handle through `fauna.actor.by_handle`. Same superset, same
/// reason — the shared UX names "handle", and an actor id pasted in should not
/// be a dead end.
///
/// ⚠ **`home_nest_url` is empty here, deliberately.** An empty url means "homed
/// on the caller's own nest", and the follower's nest relays otherwise — but the
/// *address* a user types is a handle, not a nest. Resolving the handle on our
/// own nest and following the folder it names is the same-nest case; the
/// cross-nest case arrives when the handle resolves to an actor whose folder
/// this nest relays for. Neither needs the user to know a nest url, which is why
/// the flow does not ask for one.
///
/// ⚠ **Absent, private and misspelled all fail identically.** The home nest
/// folds the three so nothing can probe for the existence of a sealed folder
/// (`folders.md` § Publicly-synced follow); this surfaces the one message rather
/// than inventing a friendlier per-case one, which would hand back exactly the
/// distinction the nest refused to make.
pub(super) async fn follow_public(
    nest: std::sync::Arc<fauna_client::NestClient>,
    follows: &dyn fauna_client_config::FollowsStore,
    handle: String,
    folder_name: String,
) -> Result<(), String> {
    fauna_client_folders::follow_ops::follow_public_folder(nest, follows, &handle, &folder_name)
        .await
        .map(|_| ())
        .map_err(fauna_client_folders::follow_ops::follow_error_text)
}

/// Unfollow — tombstone the account's row for the folder. Nothing is revoked
/// anywhere, because the home nest never knew this follower existed (the public
/// read plane keeps zero follower state by design). Idempotent.
pub(super) async fn unfollow_public(
    follows: &dyn fauna_client_config::FollowsStore,
    home_nest_url: String,
    folder_id: i64,
) -> Result<(), String> {
    fauna_client_folders::follow_ops::unfollow_public_folder(follows, &home_nest_url, folder_id)
        .await
        .map_err(|e| e.to_string())
}

/// Resolve the typed recipient, share the set (`FoldersAuthor::share_set`), then
/// re-read the roster.
///
/// **The recipient string is accepted in EITHER shape** — a bare 64-hex actor id
/// is classified locally (no network), anything else resolves as a bare
/// local-part handle through `fauna.actor.by_handle`. linux's `do_share_folder`
/// only ever does the handle lookup and apple only ever takes the actor id
/// (`actions/backups.py::share_recipient` records the split as a live priority-#1
/// divergence); taking both is the superset of the two, so tui adds no third
/// shape and works under either arm of the shared helper.
#[allow(clippy::too_many_arguments)]
pub(super) async fn share_set(
    nest: std::sync::Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
    session: std::sync::Arc<fauna_conversations::ConversationsSession>,
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    name: String,
    recipient: String,
    access: Option<String>,
) -> Result<
    (
        Vec<fauna_protocol::folders::FolderActorMember>,
        Option<String>,
    ),
    String,
> {
    let author = build_folders_author(
        std::sync::Arc::clone(&nest),
        secret,
        std::sync::Arc::clone(&folder_keys),
        mail,
        &session,
    );
    let convs = fauna_client_conversations::ConversationsClient::new(std::sync::Arc::clone(&nest));
    let recipient = recipient.trim();
    let member = match fauna_core::identity::ActorId::from_hex(recipient) {
        Ok(id) => id,
        Err(_) => {
            let resolved = convs
                .actor_by_handle(recipient.to_string())
                .await
                .map_err(|e| e.to_string())?
                .ok_or_else(|| format!("no actor for handle '{recipient}'"))?;
            fauna_core::identity::ActorId::from_hex(&resolved.actor_id)
                .map_err(|e| e.to_string())?
        }
    };
    let outcome = author
        .share_set(&convs, &name, member, None, access)
        .await
        .map_err(|e| e.to_string())?;
    let channel_id = fauna_mls::types::ChannelId(outcome.channel_id).to_string();
    let files = fauna_client_folders::FoldersClient::new(nest);
    let members = match files.actor_members_list(name).await {
        Ok(reply) => reply.members,
        Err(e) => return Err(e.to_string()),
    };
    Ok((members, Some(channel_id)))
}

/// Evict a member and ROTATE the set's content key
/// (`FoldersAuthor::remove_member` — `ui/folders.md:152`), then re-read the
/// roster. `channel_id_hex` is the set's ALREADY-DERIVED `ChannelId` (32 bytes),
/// so this decodes with `ChannelId::from_hex`, unlike the raw-group-id sites
/// above which must use [`channel_id_from_group_id_hex`].
#[allow(clippy::too_many_arguments)]
pub(super) async fn remove_member(
    nest: std::sync::Arc<fauna_client::NestClient>,
    secret: [u8; 32],
    mail: std::sync::Arc<dyn fauna_client_config::MailStore>,
    session: std::sync::Arc<fauna_conversations::ConversationsSession>,
    folder_keys: std::sync::Arc<dyn fauna_client_folders::FolderKeyStore>,
    grant_log: std::sync::Arc<dyn fauna_client_config::SuccessionLedgerStore>,
    name: String,
    channel_id_hex: String,
    member_hex: String,
) -> Result<
    (
        Vec<fauna_protocol::folders::FolderActorMember>,
        Option<String>,
    ),
    String,
> {
    // The grant log rides along for the rotation's paywall re-provision.
    let author = build_folders_author(
        std::sync::Arc::clone(&nest),
        secret,
        std::sync::Arc::clone(&folder_keys),
        mail,
        &session,
    )
    .with_grant_log(grant_log);
    let channel_id =
        fauna_mls::types::ChannelId::from_hex(&channel_id_hex).map_err(|e| e.to_string())?;
    let member = fauna_core::identity::ActorId::from_hex(&member_hex).map_err(|e| e.to_string())?;
    author
        .remove_member(&name, channel_id.0, member)
        .await
        .map_err(|e| e.to_string())?;
    let files = fauna_client_folders::FoldersClient::new(nest);
    let members = files
        .actor_members_list(name)
        .await
        .map_err(|e| e.to_string())?
        .members;
    Ok((members, Some(channel_id_hex)))
}

/// Write a member's `(access, byte_cap)` pair, then re-read the roster so the row
/// repaints from the nest's authoritative role row.
///
/// ⚠ **Both halves always ride together**: `members.set_access` upserts the whole
/// row, so sending one would silently clear the other — linux's member-row
/// handlers make the same point at both call sites.
pub(super) async fn set_member_access(
    nest: std::sync::Arc<fauna_client::NestClient>,
    name: String,
    member_hex: String,
    access: String,
    byte_cap: Option<i64>,
    channel_id_hex: Option<String>,
) -> Result<
    (
        Vec<fauna_protocol::folders::FolderActorMember>,
        Option<String>,
    ),
    String,
> {
    let files = fauna_client_folders::FoldersClient::new(nest);
    files
        .members_set_access(fauna_protocol::folders::MemberSetAccessRequest {
            name: name.clone(),
            actor_id: member_hex,
            access,
            byte_cap,
            ..Default::default()
        })
        .await
        .map_err(|e| e.to_string())?;
    let members = files
        .actor_members_list(name)
        .await
        .map_err(|e| e.to_string())?
        .members;
    Ok((members, channel_id_hex))
}

/// Accept a staged share: re-peek to resolve the Welcome bytes by id, join the
/// MLS group **off the chat rail** (no phantom chat thread), then ack.
///
/// **Join before ack, both idempotent** — an ack without a join would consume
/// the only handle on an un-joined Welcome. Runs the shared
/// `fauna_client_folders::accept_folder_share` recipe — the same one fauna-ffi's
/// `folders_accept_share`, linux and the wasm `foldersAcceptShare` twin call, so
/// that ordering is stated once (priority #2); only the join is supplied here.
pub(super) async fn accept_share(
    nest: std::sync::Arc<fauna_client::NestClient>,
    session: std::sync::Arc<fauna_conversations::ConversationsSession>,
    inbox_id: i64,
) -> Result<Vec<PendingShareView>, String> {
    let inbox = fauna_client_inbox::InboxClient::new(std::sync::Arc::clone(&nest));
    fauna_client_folders::accept_folder_share(&inbox, inbox_id, |join| async move {
        let (channel_id_hex, welcome_bytes, home_nest_url, welcome_ctx) = join.into_join_args();
        session
            .join_folder_welcome(channel_id_hex, welcome_bytes, home_nest_url, welcome_ctx)
            .await
            .map(|_| ())
    })
    .await
    .map_err(|e| e.to_string())?;
    Ok(load_pending_shares(nest).await)
}

/// Decline a staged share through the SHARED
/// `fauna_client_folders::decline_folder_share` recipe — the same one the FFI
/// and wasm twins call, so the roster-drop-then-ack ordering is stated once
/// (priority #2). Declining drops the caller's roster row as well as acking, so
/// the owner's "Shared with" list stops over-reporting and a later re-share is a
/// genuine re-invite (`ui/folders.md:160`).
pub(super) async fn decline_share(
    nest: std::sync::Arc<fauna_client::NestClient>,
    inbox_id: i64,
) -> Result<Vec<PendingShareView>, String> {
    fauna_client_folders::decline_folder_share(
        &fauna_client_inbox::InboxClient::new(std::sync::Arc::clone(&nest)),
        &fauna_client_folders::FoldersClient::new(std::sync::Arc::clone(&nest)),
        inbox_id,
    )
    .await
    .map_err(|e| e.to_string())?;
    Ok(load_pending_shares(nest).await)
}

/// Leave a set shared *with* this user, addressed by the raw `mls_group_id` hex
/// the member row carries. Two idempotent, self-scoped halves, **nest roster-drop
/// FIRST**: off the roster their `content_key.get` folds to `not_found`, so a
/// failure after it leaves local state untouched and a retry is clean. No
/// content-key rotation — a voluntary leaver keeps the generations they held
/// (`ui/folders.md:178`, mls-group-key-material § M2).
pub(super) async fn leave_share(
    nest: std::sync::Arc<fauna_client::NestClient>,
    session: std::sync::Arc<fauna_conversations::ConversationsSession>,
    group_id: String,
) -> Result<(), String> {
    fauna_client_folders::leave_share(nest, &session, group_id).await
}

/// The ordered ui.yaml `folders` element list. While a creation wizard is
/// open ([`fauna_devices_machine::DevicesSnapshot::wizard`] is `Some`), it is
/// the WHOLE page (the linux/windows modal-dialog shape); otherwise the list +
/// in-place config + conflict review list render.
pub(super) fn folders_elements(
    state: &SettingsState,
    locations: &[RenderedLocationBinding],
) -> Vec<Element> {
    let mut els = vec![Element::label(ids::PAGE_HEADING, t::FOLDERS)];
    let snapshot = state.devices.snapshot.as_ref();

    if let Some(wizard) = snapshot.and_then(|s| s.wizard.as_ref()) {
        els.extend(wizard_elements(wizard));
        return els;
    }

    // The headless-deployment offer, above the set list (placement user-approved
    // 2026-07-23): the user binds a folder here, and this is what keeps it
    // syncing once they disconnect. Rendered only where the question applies —
    // `None` is a machine with no lingering concept (`sync-agent.md` § Headless
    // deployment). The help line is untagged chrome, like `serve-here`'s
    // subtitle: ui.yaml gives it no id, and inventing one is the anti-pattern.
    if let Some(on) = state.folders.linger_enabled {
        els.push(
            Element::checkbox_gesture(
                ids::SYNC_AGENT_LINGER_TOGGLE,
                fs_strings::KEEP_SYNCING_WHEN_LOGGED_OUT,
                on,
                Gesture::Settings(Action::ToggleSyncAgentLinger),
            )
            .attr("state", if on { "on" } else { "off" }),
        );
        els.push(Element::chrome(if on {
            fs_strings::KEEP_SYNCING_HELP_ON
        } else {
            fs_strings::KEEP_SYNCING_HELP_OFF
        }));
    }

    // The page-level "Sync defaults" section — one control today, the default
    // conflict policy stamped onto NEW sets (`ui/folders.md` § Element IDs;
    // user-approved home 2026-07-11). Unindexed, unlike every per-set select
    // below it. Rendered unconditionally: absent/`None` shows `auto`, which is
    // exactly what a new set would get, so there is no "unknown" state to hide.
    els.push(Element::chrome(t::SYNC_DEFAULTS));
    els.push(
        Element::select(
            ids::SYNC_DEFAULT_CONFLICT_POLICY_SELECT,
            state
                .folders
                .default_conflict_policy
                .clone()
                .unwrap_or_else(|| "auto".into()),
            SelectTarget::FolderDefaultConflictPolicy,
            conflict_policy_options()
                .into_iter()
                .map(|o| o.value)
                .collect(),
        )
        .labelled(t::DEFAULT_CONFLICT_POLICY),
    );

    els.push(Element::gesture_button(
        ids::FOLDER_ADD_BUTTON,
        t::ADD_FOLDER,
        true,
        Gesture::Settings(Action::OpenFolderWizard),
    ));

    // The recipient-side "Shared with you" knock list — a page-level section
    // above the set list, not a per-row surface (`ui/folders.md:168`). Only a
    // STRANGER's share lands here: a Confirmed/Accepted contact's share
    // auto-joins off the chat rail and goes straight into the set list. The
    // section vanishes when empty rather than showing an empty-state, matching
    // linux — an absent knock list is the normal state.
    els.extend(pending_share_elements(&state.folders.pending_shares));

    // The ceremony's consent cards CONTINUE that same indexed family rather
    // than opening a second one — ui.yaml declares `folder-pending-share` as
    // "a knocked M2 share …, OR an offline-ceremony group invitation", so a
    // user (and the shared suite) sees one list of things awaiting an answer.
    // The index continues from the M2 rows, which is what keeps
    // `folder-pending-share[k] / folder-share-accept-button` resolving to the
    // card it is drawn under.
    #[cfg(feature = "p2p-share")]
    els.extend(group_invitation_elements(
        &state.folders.group_shares.invitations,
        state.folders.pending_shares.len(),
    ));

    // The co-present ceremony sits directly under the knock list, because the
    // two are the same story from opposite ends: a share that reached you
    // through your nest, and one being handed to you in person.
    #[cfg(feature = "p2p-share")]
    els.extend(offline_share_elements(&state.offline_share));

    // And the transfer surface directly under that: what this device is
    // serving to peers and what the pump moved — the plane the ceremony
    // above initiates into.
    #[cfg(feature = "p2p-share")]
    els.extend(share_transfer_elements(state.share_plane.as_ref()));

    let folders = snapshot.map(|s| s.folders.as_slice()).unwrap_or(&[]);
    // The website hint's second half — the actor's web-address opt-in, read
    // best-effort with the page (`None` = unknown, the hedged wording).
    let website_address_enabled = snapshot.and_then(|s| s.website_address_enabled);
    // The unsealed device roster names each `folder-place-row`.
    let devices = snapshot.map(|s| s.devices.as_slice()).unwrap_or(&[]);
    // The row the lease-status line calls "this device" — the same answer the
    // Devices page's `device-this-mark-badge` marks (`this_device_row` owns
    // the enrolled-row-else-own-id rule).
    let this_row = fauna_devices_machine::this_device_row(
        state.devices.enrolled_device_row.as_deref(),
        state.devices.local_device_id.as_deref(),
    );
    let now = lease_clock_now();
    // This seat's own actor id — the trust anchor the owner's rows are judged
    // under (`FolderSummary::is_public_unverified_for`). Derived once per paint;
    // `None` pre-login, when the re-confirm status simply does not paint.
    let own_actor = state
        .identity_secret()
        .map(|secret| fauna_core::identity::ActorKeypair::from_secret(secret).actor_id());
    for (i, fs) in folders.iter().enumerate() {
        // Resolved here, off the projection this page already read — never by
        // asking the nest (`ui/folders.md` § Exclusive editing: the acquire
        // kind takes the lease as a side effect of asking).
        let lease_status =
            fauna_devices_machine::folder_lease_status(fs, devices, this_row.as_deref(), now)
                .map(|text| text.resolve(fauna_i18n::strings::lookup));
        els.extend(folder_row_elements(
            fs,
            i,
            &state.folders,
            devices,
            locations,
            website_address_enabled,
            lease_status,
            own_actor.as_ref(),
        ));
    }

    // Group scopes are sets like any other, so they are `folder-row`s like any
    // other — appended after the M2 sets, continuing the index. They carry no
    // per-row body in v1: a group scope has no local seat config to edit and
    // no name to rename (naming kinds arrive with group content-kind sealing),
    // so what a row owes is exactly what it can honestly show — the set, and
    // who it is shared with.
    #[cfg(feature = "p2p-share")]
    els.extend(group_scope_row_elements(
        &state.folders.group_shares.scopes,
        folders.len(),
    ));

    // "Folders you follow", below the user's OWN folders: these are somebody
    // else's, read-only, and the section is always offered (the follow button is
    // how a user gets their first one, so it cannot be gated on already having
    // some).
    els.extend(followed_elements(
        snapshot.map(|s| s.followed.as_slice()).unwrap_or(&[]),
        &state.folders,
    ));

    let conflicts = snapshot.map(|s| s.conflicts.as_slice()).unwrap_or(&[]);
    for c in conflicts {
        els.extend(conflict_elements(c));
    }

    // `settings-nav-back` — Esc already returns to the Settings hub
    // (`Action::NavBack => state.sub = SubPage::Root`, app.rs), but a live user
    // report found it was the ONLY way out, undiscoverable (user-approved
    // 2026-08-03; matches `account.rs`/`privacy.rs`'s existing pattern).
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );

    els
}

/// One `folder-row` (indexed) plus, for an OWNER row, its per-row
/// `folder-conflict-policy-select` — rendered in what linux calls the
/// "collapsed header", above the expander gate. A `role == "member"` row gets
/// none of it: policy stays owner-only (`ui/folders.md:152`) — see the
/// recipient-surfaces block below. (The per-row scan-frequency select that
/// used to sit beside it retired with phase 5: the cadence is a constant.)
///
/// The path editors, delete affordance, webdav/paywall controls, and the
/// "Shared with" roster render only for the one expanded OWNER row. The
/// device-local `folder-location-*` binding section is the one piece of the
/// expanded body a member row DOES get, and only with `access == "writer"`
/// (mirrors linux's `build_writer_member_folder_row`) — a reader stays
/// read-only, nothing to expand into. `locations` is the whole rendered binding
/// set, filtered to this row's set by [`location_elements`].
#[allow(clippy::too_many_arguments)]
fn folder_row_elements(
    fs: &FolderSummary,
    i: usize,
    ui: &FoldersUiState,
    devices: &[fauna_devices_machine::DeviceSummary],
    locations: &[RenderedLocationBinding],
    website_address_enabled: Option<bool>,
    lease_status: Option<String>,
    own_actor: Option<&fauna_core::identity::ActorId>,
) -> Vec<Element> {
    let is_member = fs.role.as_deref() == Some("member");

    let mut els = vec![Element::gesture_button(
        ids::FOLDER_ROW,
        fs.name.clone(),
        true,
        Gesture::Settings(Action::ToggleFolderRow(i)),
    )];
    // On EVERY owner row — a folder has no type, so the former sync-type gate
    // retired with the mode (`ui/folders.md` § Conflicts).
    if !is_member {
        // Prompt + human display: a bare `< auto >` shows the wire value
        // with nothing naming the control (copy-audit corpus, 2026-08-04);
        // the wire value still round-trips through `select`.
        let policy = fs.conflict_policy.clone().unwrap_or_else(|| "auto".into());
        els.push(
            Element::select(
                ids::FOLDER_CONFLICT_POLICY_SELECT,
                policy.clone(),
                SelectTarget::FolderConflictPolicy { row: i },
                conflict_policy_options()
                    .into_iter()
                    .map(|o| o.value)
                    .collect(),
            )
            .labelled(t::CONFLICT_POLICY)
            .display_value(
                fauna_folders_machine::conflict_policy_label(&policy)
                    .resolve(fauna_i18n::strings::lookup),
            )
            .within(ids::FOLDER_ROW, i),
        );
    }

    // ── The RECIPIENT surfaces, on the row HEADER (not the expanded body) ──
    //
    // A `role == "member"` row is a set shared WITH this user: read-only, badged
    // "Shared by ‹who›" over the pre-computed `owner_display`, and carrying the
    // self-scoped `folder-leave-button` (`ui/folders.md:167`). Header rather
    // than body because the shared suite reads both WITHOUT expanding the row
    // (`test_folder_pending_share_accept_decline` asserts them straight after
    // the set appears) — linux likewise hangs them off the row itself.
    //
    // ⚠ A member row only reaches this render at all once the B3 join-filter is
    // wired (`super::devices::wire_mls_query`): `DevicesMachine` drops every
    // rostered-but-un-joined row, which is what stops a stranger's knock from
    // forcing a set into the list.
    if fs.role.as_deref() == Some("member") {
        // NEVER re-derive the handle-else-short-id fallback locally — the label
        // is pre-computed once in the shared transcribe so the seven apps cannot
        // drift on it (`ui/folders.md:150`, value-formatting.md § Account
        // display label).
        els.push(
            Element::label(ids::FOLDER_SHARED_BADGE, t::shared_by(&fs.owner_display))
                .within(ids::FOLDER_ROW, i),
        );
        // No group id ⇒ nothing to address the leave by; withhold the affordance
        // rather than offer one that cannot work (a member row always carries
        // one, so this is belt-and-braces). The gesture carries only the row
        // index and re-resolves the id off the snapshot, keeping `Action` cheap.
        if fs.mls_group_id.is_some() {
            els.push(
                Element::gesture_button(
                    ids::FOLDER_LEAVE_BUTTON,
                    common::LEAVE,
                    true,
                    Gesture::Settings(Action::LeaveFolderShare(i)),
                )
                .within(ids::FOLDER_ROW, i),
            );
        }
    }

    // ── Exclusive editing's status line, on the row HEADER ──
    //
    // Header rather than body for the same reason as the member badge above: a
    // reader member has no expanded body at all, and `ui/folders.md`
    // § Exclusive editing has the reader see the lock too — the seat that most
    // needs to know it cannot upload. Present only while the folder is
    // lease-governed (`folder_lease_status` answers `None` otherwise).
    if let Some(text) = lease_status {
        els.push(Element::label(ids::FOLDER_LEASE_STATUS, text).within(ids::FOLDER_ROW, i));
    }

    if ui.expanded == Some(i) {
        // Everything the expander body holds is a DESCENDANT of this row — on
        // linux/windows/macOS literally (the `AdwExpanderRow`/`Expander` subtree),
        // so the shared suite addresses it as `scope="folder-row[i]"`
        // (`test_folders.py::test_bind_location_nested_under_folder`,
        // `actions/backups.py`'s row-scoped reads). tui has no widget tree, so the
        // containment has to be declared: every body element is registered
        // `.within(ids::FOLDER_ROW, i)`, exactly like the two header selects above.
        // A top-level element never matches a scoped query (`automation.rs`
        // `Registry::matches` — an empty path contains no container), so
        // registering the body
        // flat is what makes a scoped count come back 0 even though the element
        // painted. `within` PREPENDS, so `location_elements`' own
        // `.within(ids::FOLDER_LOCATION_ROW, k)` children compose into
        // `folder-row[i] / folder-location-row[k]` rather than being overwritten.
        let mut body = Vec::new();
        if !is_member {
            body.push(
                Element::input(
                    ids::FOLDER_INCLUDE_PATHS,
                    ui.include_paths_input.clone(),
                    Field::Settings(SettingsField::FolderIncludePaths),
                )
                .labelled(t::INCLUDE_PATHS),
            );
            body.push(
                Element::input(
                    ids::FOLDER_EXCLUDE_PATHS,
                    ui.exclude_paths_input.clone(),
                    Field::Settings(SettingsField::FolderExcludePaths),
                )
                .labelled(t::EXCLUDE_PATHS),
            );
            body.push(Element::gesture_button(
                ids::FOLDER_SAVE_PATHS,
                t::SAVE_PATHS,
                true,
                Gesture::Settings(Action::SaveFolderPaths),
            ));
            // ── The nest place's snapshot policy (backup-restore.md § 8b) ──
            // On EVERY folder, not just a backup-type one: the mode is gone and
            // "what the nest keeps" is a property of the one place every folder
            // has. Three knobs, each three-state — the select spells the third
            // state out; a blank box IS the third state for the other two.
            body.push(Element::chrome(t::NEST_PLACE_SECTION));
            body.push(
                Element::select(
                    ids::FOLDER_NEST_SNAPSHOTS_SELECT,
                    ui.nest_snapshots_input.clone(),
                    SelectTarget::FolderNestSnapshots { row: i },
                    nest_snapshots_options()
                        .into_iter()
                        .map(|o| o.value)
                        .collect(),
                )
                .display_value(
                    nest_snapshots_label(&ui.nest_snapshots_input)
                        .resolve(fauna_i18n::strings::lookup),
                )
                .labelled(t::NEST_SNAPSHOTS),
            );
            body.push(
                Element::input(
                    ids::FOLDER_NEST_QUIET_INPUT,
                    ui.nest_quiet_input.clone(),
                    Field::Settings(SettingsField::FolderNestQuiet),
                )
                .labelled(t::NEST_QUIET),
            );
            body.push(
                Element::input(
                    ids::FOLDER_NEST_RETENTION_SNAPSHOTS,
                    ui.nest_retention_snapshots_input.clone(),
                    Field::Settings(SettingsField::FolderNestRetentionSnapshots),
                )
                .labelled(t::NEST_RETENTION_SNAPSHOTS),
            );
            body.push(
                Element::input(
                    ids::FOLDER_NEST_RETENTION_DAYS,
                    ui.nest_retention_days_input.clone(),
                    Field::Settings(SettingsField::FolderNestRetentionDays),
                )
                .labelled(t::NEST_RETENTION_DAYS),
            );
            // The version-retention SIBLING pair (file-versions.md § Retention
            // ruling 1): bounds file-version history, never snapshots — its
            // own `folders.version_retention` column, riding the same save.
            body.push(
                Element::input(
                    ids::FOLDER_VERSION_RETENTION_COUNT,
                    ui.version_retention_count_input.clone(),
                    Field::Settings(SettingsField::FolderVersionRetentionCount),
                )
                .labelled(t::VERSION_RETENTION_COUNT),
            );
            body.push(
                Element::input(
                    ids::FOLDER_VERSION_RETENTION_DAYS,
                    ui.version_retention_days_input.clone(),
                    Field::Settings(SettingsField::FolderVersionRetentionDays),
                )
                .labelled(t::VERSION_RETENTION_DAYS),
            );
            // Blank means "use the default" for all three text knobs, and the
            // save sends the policy WHOLE — so this line is not decoration: it
            // is the only on-screen statement that emptying a box is a real
            // choice rather than a no-op.
            body.push(Element::chrome(t::NEST_PLACE_BLANK_HINT));
            body.push(Element::gesture_button(
                ids::FOLDER_NEST_SAVE_BUTTON,
                t::NEST_SAVE,
                true,
                Gesture::Settings(Action::SaveFolderNestPlace(i)),
            ));
            // ── The nest place's content residency (phase 5 — file-sync.md
            // § Content residency). Applies on change like the audience
            // select, and like it the destructive direction only ARMS a
            // confirm: Metadata-only deletes the nest's copy of the folder's
            // content. Its own `folders.update` field, deliberately outside
            // the sent-whole `nest_place` save above — an older writer's
            // policy edit must never silently clear it.
            let residency = fauna_folders_machine::normalize_residency(&fs.residency);
            body.push(
                Element::select(
                    ids::FOLDER_NEST_RESIDENCY_SELECT,
                    residency.clone(),
                    SelectTarget::FolderNestResidency { row: i },
                    fauna_folders_machine::residency_options()
                        .into_iter()
                        .map(|o| o.value)
                        .collect(),
                )
                .labelled(t::FOLDER_RESIDENCY)
                .display_value(
                    fauna_folders_machine::residency_label(&residency)
                        .resolve(fauna_i18n::strings::lookup),
                ),
            );
            // The hint rides the same normalized value the select paints, so
            // copy and control cannot disagree: a full folder says what the
            // nest's copy buys, a metadata-only one states the availability
            // cost it accepted.
            body.push(Element::chrome(
                fauna_folders_machine::residency_hint(&residency)
                    .resolve(fauna_i18n::strings::lookup),
            ));
            if ui.residency_pending {
                body.push(Element::chrome(t::RESIDENCY_CONFIRM_TITLE));
                body.push(Element::chrome(t::RESIDENCY_CONFIRM_BODY));
                body.push(Element::gesture_button(
                    ids::FOLDER_RESIDENCY_CONFIRM,
                    t::RESIDENCY_CONFIRM,
                    true,
                    Gesture::Settings(Action::ConfirmFolderMetadataOnly(i)),
                ));
            }
            // ── Exclusive editing (file-sync.md § Exclusive editing) — the
            // owner's per-folder "one device at a time" opt-in. Its own
            // `folders.update` field; non-optimistic, like the website toggle:
            // the row repaints the nest's `exclusive_editing` after the write.
            let exclusive = fs.exclusive_editing;
            body.push(
                Element::checkbox_gesture(
                    ids::FOLDER_EXCLUSIVE_EDITING_TOGGLE,
                    t::FOLDER_EXCLUSIVE_EDITING,
                    exclusive,
                    Gesture::Settings(Action::ToggleFolderExclusiveEditing(i)),
                )
                .attr("state", if exclusive { "on" } else { "off" }),
            );
            // ── The post-create device-place editor (phase 2; the
            // `folder-place-*` IDs were user-approved with slice e's grant and
            // sat unbuilt on every app until now). Each of the folder's DEVICE
            // seats renders as a `folder-place-row` with its three flag
            // checkboxes, edited in place through `fauna.folders.places.set` —
            // the same boxes and labels the wizard's enrollment step paints,
            // on a live seat. The fresh guard mirrors `roster_for`: a
            // stale list from a previously expanded row never paints here.
            if ui.device_places_for.as_deref() == Some(fs.name.as_str())
                && !ui.device_places.is_empty()
            {
                body.push(Element::chrome(t::FOLDER_PLACES_TITLE));
                // One shared projection, not a per-app read of the roster:
                // `place_rows` reads each seat's flags and names it from the
                // unsealed device roster — `members.list` carries no
                // plaintext name for a user-named device
                // (`fauna_protocol::folders::place_rows`, whose docs carry
                // why each arm is a trap). The six trickle-down legs paint from
                // the same rows.
                for (j, place) in fauna_devices_machine::place_rows(&ui.device_places, devices)
                    .iter()
                    .enumerate()
                {
                    // The row root is the scope its children hang under
                    // (`folder-place-row[j] / folder-place-originates` — the
                    // ui.yaml registry names exactly this shape).
                    body.push(Element::label(ids::FOLDER_PLACE_ROW, place.label.clone()));
                    for (id, label, on, flag) in [
                        (
                            ids::FOLDER_PLACE_ORIGINATES,
                            t::wizard::PLACE_ORIGINATES,
                            place.originates,
                            PlaceFlag::Originates,
                        ),
                        (
                            ids::FOLDER_PLACE_ACCEPTS,
                            t::wizard::PLACE_ACCEPTS,
                            place.accepts,
                            PlaceFlag::Accepts,
                        ),
                        (
                            ids::FOLDER_PLACE_APPLIES_DELETES,
                            t::wizard::PLACE_APPLIES_DELETES,
                            place.applies_deletes,
                            PlaceFlag::AppliesDeletes,
                        ),
                    ] {
                        body.push(
                            Element::checkbox_gesture(
                                id,
                                label,
                                on,
                                Gesture::Settings(Action::ToggleFolderPlaceFlag {
                                    row: i,
                                    place: j,
                                    flag,
                                }),
                            )
                            // The `get_attr("state")` contract every flag
                            // checkbox carries (the wizard's own comment).
                            .attr("state", if on { "on" } else { "off" })
                            .within(ids::FOLDER_PLACE_ROW, j),
                        );
                    }
                }
            }
            // ── Destination places (backup-destinations.md § Ordinary-folder
            // coverage; IDs user-approved 2026-08-20, tui leads). Owner rows
            // only — coverage is owner-plane state, and the resolver behind
            // the load already refuses a member row. The reserved rails never
            // appear here: the folder list itself excludes `__` sets. The
            // fresh guard mirrors `device_places_for`, and the section is
            // absent entirely while the owner has no enrolled destination —
            // an affordance that cannot work must not paint.
            if ui.destination_places_for.as_deref() == Some(fs.name.as_str())
                && !ui.destination_places.is_empty()
            {
                body.push(Element::chrome(t::FOLDER_DESTINATIONS_TITLE));
                for (attached_index, place) in ui
                    .destination_places
                    .iter()
                    .filter(|p| p.attached)
                    .enumerate()
                {
                    body.push(Element::label(
                        ids::FOLDER_DESTINATION_ROW,
                        place.label.clone(),
                    ));
                    body.push(
                        Element::gesture_button(
                            ids::FOLDER_DESTINATION_DETACH_BUTTON,
                            t::FOLDER_DESTINATION_DETACH,
                            true,
                            Gesture::Settings(Action::DetachFolderDestination {
                                row: i,
                                place: attached_index,
                            }),
                        )
                        .within(ids::FOLDER_DESTINATION_ROW, attached_index),
                    );
                }
                let attachable: Vec<&FolderDestinationPlace> = ui
                    .destination_places
                    .iter()
                    .filter(|p| !p.attached)
                    .collect();
                if !attachable.is_empty() {
                    let selection = &ui.destination_attach_selection;
                    let display = attachable
                        .iter()
                        .find(|p| &p.destination_id == selection)
                        .map(|p| p.label.clone())
                        .unwrap_or_default();
                    body.push(
                        Element::select(
                            ids::FOLDER_DESTINATION_ATTACH_SELECT,
                            selection.clone(),
                            SelectTarget::FolderDestinationAttach { row: i },
                            attachable
                                .iter()
                                .map(|p| p.destination_id.clone())
                                .collect(),
                        )
                        .display_value(display)
                        .labelled(t::FOLDER_DESTINATION_ATTACH_LABEL),
                    );
                    body.push(Element::gesture_button(
                        ids::FOLDER_DESTINATION_ATTACH_BUTTON,
                        t::FOLDER_DESTINATION_ATTACH,
                        !selection.is_empty(),
                        Gesture::Settings(Action::AttachFolderDestination { row: i }),
                    ));
                }
            }
            body.push(Element::gesture_button(
                ids::FOLDER_DELETE_BUTTON,
                t::DELETE_FOLDER,
                true,
                Gesture::Settings(Action::OpenFolderDeleteConfirm),
            ));
            if ui.delete_pending {
                body.push(Element::gesture_button(
                    ids::FOLDER_DELETE_CONFIRM,
                    common::DELETE,
                    true,
                    Gesture::Settings(Action::ConfirmFolderDelete),
                ));
            }
            // ── Audience + website serving (phase 4 slice 4d) ──────────────
            //
            // Both render on EVERY owner row, unconditional on mode — unlike the
            // WebDAV/paywall pair below, which are per-mode. Audience is the
            // folder's identity rather than a serving option (`folders.md`
            // § Target re-model), and the website toggle is the ONLY door to a
            // website folder now that phase 2 slice e retired the wizard's mode
            // step — gating either behind `fs.mode` would re-create the very gap
            // this slice closes.
            let bound = fs.mls_group_id.is_some();
            // Normalized, never the raw column: the select's value has to be one
            // of the options it offers, and a folder summary with no audience sends none at all
            // (`FolderSummary::default` leaves it empty). Fail-closed — an
            // unparseable value never paints as Public.
            let audience = fauna_folders_machine::normalize_audience(&fs.audience, bound);
            // The option set rides (bound, current): a bound folder's `shared`
            // is selectable exactly while it is `public` — the
            // flip-back, the one exit from its public window.
            let audience_opts = fauna_folders_machine::audience_options(bound, &audience);
            body.push(
                Element::select(
                    ids::FOLDER_AUDIENCE_SELECT,
                    audience.clone(),
                    SelectTarget::FolderAudience { row: i },
                    audience_opts.iter().map(|o| o.value.clone()).collect(),
                )
                .labelled(t::FOLDER_AUDIENCE)
                .display_value(
                    fauna_folders_machine::audience_label(&audience)
                        .resolve(fauna_i18n::strings::lookup),
                ),
            );
            // The hint rides the same (bound, current) inputs as the option
            // set, so the copy and the picker cannot disagree: a bound folder
            // explains that sharing is edited below (`private` is refused
            // nest-side while bound), and a bound folder currently PUBLIC
            // explains the one exit its picker does offer — picking Shared
            // re-seals it for its members.
            body.push(Element::label(
                ids::FOLDER_AUDIENCE_HINT,
                fauna_folders_machine::audience_hint(bound, &audience)
                    .resolve(fauna_i18n::strings::lookup),
            ));
            // The owner's re-confirm door: public on the nest's say-so, but the
            // attestation does not verify under this seat's own id (inherited
            // through a succession, or otherwise
            // not verifying). Such seats keep it sealed until the
            // owner re-confirms, and a select already at `public` fires no
            // change — so the button arms the SAME declassify confirm the
            // select does, whose answer re-mints through `set_audience`
            // (`encryption-at-rest.md` § Readable classes → *The
            // declassification is owner-ATTESTED*).
            if own_actor.is_some_and(|own| fs.is_public_unverified_for(own)) {
                body.push(Element::label(
                    ids::FOLDER_AUDIENCE_UNATTESTED,
                    t::FOLDER_AUDIENCE_UNATTESTED,
                ));
                if !ui.audience_public_pending {
                    body.push(Element::gesture_button(
                        ids::FOLDER_AUDIENCE_RECONFIRM_BUTTON,
                        t::FOLDER_AUDIENCE_RECONFIRM,
                        true,
                        Gesture::Settings(Action::SetFolderAudience {
                            row: i,
                            audience: fauna_protocol::folders::AUDIENCE_PUBLIC.to_string(),
                        }),
                    ));
                }
            }
            // The declassify dialog — armed by picking `public`, committed only
            // here. All three copy lines paint, because the two consequences are
            // separately surprising: names and paths go public too (they are
            // URLs), and the flip back re-seals only FUTURE content.
            if ui.audience_public_pending {
                body.push(Element::chrome(t::DECLASSIFY_TITLE));
                body.push(Element::label(
                    ids::FOLDER_AUDIENCE_PUBLIC_NAMES_WARNING,
                    t::DECLASSIFY_BODY,
                ));
                body.push(Element::label(
                    ids::FOLDER_AUDIENCE_PUBLIC_RESEAL_WARNING,
                    t::DECLASSIFY_IRREVERSIBLE,
                ));
                body.push(Element::gesture_button(
                    ids::FOLDER_AUDIENCE_PUBLIC_CONFIRM,
                    t::DECLASSIFY_CONFIRM,
                    true,
                    Gesture::Settings(Action::ConfirmFolderPublic(i)),
                ));
            }
            // "Serve as your website" — the structural sibling of the WebDAV
            // toggle below. It is ALLOWED while the folder is neither public nor
            // paywalled, and inert there: the flag publishes the head, the
            // audience decides who may read it. So this hints rather than
            // disabling — a disabled control would imply the setting is
            // unavailable, when it is merely not yet visible to anyone.
            // The wording is the shared tri-state on the LIVE serving picture
            // (`website_serve_hint` owns the states + the degrade direction).
            let website_hint = fauna_folders_machine::website_serve_hint(
                &audience,
                fs.web_paywall_tier.is_some(),
                website_address_enabled,
            )
            .resolve(fauna_i18n::strings::lookup);
            body.push(
                Element::checkbox_gesture(
                    ids::FOLDER_WEBSITE_TOGGLE,
                    t::SERVE_WEBSITE,
                    fs.website_enabled,
                    Gesture::Settings(Action::ToggleFolderWebsite(i)),
                )
                .labelled(website_hint.clone())
                .attr("state", if fs.website_enabled { "on" } else { "off" }),
            );
            // As the WebDAV toggle just below: a checkbox's `label` is
            // driver-visible only, so the hint ALSO paints as the adjacent line
            // `folder-website-hint` names.
            body.push(Element::label(ids::FOLDER_WEBSITE_HINT, website_hint));
            // Per-set "serve over WebDAV" — every owner row (a reserved `__`
            // rail is never listed here, and the nest refuses serving one —
            // `webdav-server.md` § What the namespace is), in the expanded
            // body (linux's `BuildWebdavToggleRow` placement; the shared e2e
            // helper reads it on the expanded row). DISABLED, with a "set up
            // mail first" hint in place of the usual one, while the actor holds
            // no MSEK — see `FoldersUiState::can_serve_webdav` for why that is
            // a gate and not a message. `ui/folders.md:59`.
            if !is_member {
                let hint = if ui.can_serve_webdav {
                    t::SERVE_WEBDAV_HINT
                } else {
                    t::SERVE_WEBDAV_NEEDS_MAIL
                };
                body.push(
                    Element::checkbox_gesture(
                        ids::FOLDER_WEBDAV_TOGGLE,
                        t::SERVE_WEBDAV,
                        fs.webdav_enabled,
                        Gesture::Settings(Action::ToggleFolderWebdav(i)),
                    )
                    .enabled(ui.can_serve_webdav)
                    .labelled(hint)
                    .attr("state", if fs.webdav_enabled { "on" } else { "off" }),
                );
                // A checkbox's `label` is driver-visible only (the paint is
                // `[x] text` — ui.rs's checkbox arm), so the hint ALSO paints as
                // the adjacent chrome line every other toggle uses — without it
                // a human sees a dead control with no reason while the e2e
                // reads one (copy-audit corpus, 2026-08-04; vocabulary rule 3).
                body.push(Element::chrome(hint));
            }
            // Per-set "paywall to tier" — website-enabled rows ONLY (keyed on
            // the toggle above, never on the retired `mode = "web"` spelling;
            // the structural sibling of the WebDAV toggle). Options are the creator's
            // OWN tiers; with none, the select is disabled with a "create a
            // tier first" hint, since there is nothing to paywall to. v1 is
            // SET-ONLY: the "Not paywalled" placeholder is offered only while
            // the set is still public, and no client yet ships a clear control
            // (`ui/folders.md:60`).
            if fs.website_enabled {
                let paywalled = fs.web_paywall_tier.clone();
                let mut options: Vec<String> = Vec::new();
                if paywalled.is_none() {
                    options.push(String::new());
                }
                options.extend(ui.own_tiers.iter().cloned());
                body.push(
                    Element::select(
                        ids::FOLDER_PAYWALL_TIER_SELECT,
                        paywalled.clone().unwrap_or_default(),
                        SelectTarget::FolderPaywallTier { row: i },
                        options,
                    )
                    .enabled(!ui.own_tiers.is_empty())
                    .labelled(if ui.own_tiers.is_empty() {
                        t::PAYWALL_TIER_NEEDS_TIER
                    } else {
                        t::PAYWALL_TIER
                    }),
                );
            }
            // Per-set device activity — the ordinary sync "who has
            // recorded a change" signal (`fauna.folders.devices`), OWNER
            // rows only, unconditional on mode (mirrors web's
            // `FoldersSection.svelte`, which paints it in every expanded
            // owner row regardless of Sync/Backup/Web — `file-sync.md` §
            // Implementation status today). Distinct from the cross-user
            // "Shared with" roster right below it.
            body.extend(device_activity_elements(fs, ui));
            // The owner-side "Shared with" section — OWNER rows only. Sharing
            // is owner-only (`ui/folders.md:152`): a member cannot re-share,
            // and the nest enforces it, so
            // offering the affordance on a member row would only produce a
            // refusal.
            body.extend(shared_with_elements(fs, i, ui));
        }
        // The device-local folder-binding section — OWNER rows, WRITER member
        // rows, and any member row with a PARKED binding (`ui/folders.md:152`;
        // the one shared decision, `fauna_folders_machine::binding_section`): a
        // reader browses + decrypts via Media, never binds a folder (a bound
        // folder whose edits cannot upload would breach `file-sync.md`'s iron
        // rule), so a reader-access member row with nothing parked gets nothing
        // here, matching linux's `build_member_folder_row`.
        let parked = locations
            .iter()
            .any(|b| b.folder == fs.name && b.access_revoked);
        let section = fauna_folders_machine::binding_section(
            fs.role.as_deref(),
            fs.access.as_deref(),
            parked,
        );
        if section.shown {
            // `folder-access-revoked-warning` (D4) — the owner withdrew this
            // actor's write grant mid-life, the authoritative nest refused the
            // next mint/record, and the agent parked every folder bound to the
            // set. Rendered ABOVE the binding widget, worded to say both halves
            // (sync stopped; local files untouched) so the user never discovers
            // months later that a folder they believed was syncing simply went
            // quiet. By then the row's access reads `reader`, which is why the
            // section keys on the park and not only on the access. The binding
            // rows stay visible and removable below — a park is not a deletion.
            if section.revoked_warning {
                body.push(Element::label(
                    ids::FOLDER_ACCESS_REVOKED_WARNING,
                    t::ACCESS_REVOKED_WARNING,
                ));
            }
            body.extend(location_elements(fs, ui, locations));
        }
        els.extend(body.into_iter().map(|e| e.within(ids::FOLDER_ROW, i)));
    }
    els
}

/// The co-present **offline share initiation** affordance (`p2p.md` § Offline
/// share initiation, contract point 1) — the eight rule-A-approved
/// `offline-{share,receive}-*` elements, page-level like the knock list above.
///
/// Every paint decision comes from the SHARED projection
/// (`fauna_client_capabilities::group_ceremony_view::OfflineShareView`), never
/// from tui state directly: when Begin is clickable and what counts as a valid
/// compare code are security-relevant, and seven apps deciding them
/// separately is seven chances to disagree. This function only maps that
/// projection onto tui widgets and i18n strings.
///
/// **The whole section vanishes when the affordance is unavailable** — no
/// usable identity secret, or rule 7's brake refused the listener. An
/// affordance that cannot work is worse than an absent one, and a "Share a
/// folder" button that always errors is exactly the stub the standing
/// tui-parity rule forbids.
#[cfg(feature = "p2p-share")]
fn offline_share_elements(state: &crate::offline_share::OfflineShareState) -> Vec<Element> {
    use fauna_client_capabilities::group_ceremony_view::OfflineSharePanel;

    let view = state.view();
    if !view.available {
        return Vec::new();
    }
    let mut els = vec![Element::chrome(fs_strings::OFFLINE_SHARE_SECTION)];

    if view.shows_entry_buttons() {
        els.push(Element::gesture_button(
            ids::OFFLINE_SHARE_BUTTON,
            fs_strings::OFFLINE_SHARE_START,
            true,
            Gesture::Settings(Action::OpenOfflineShare),
        ));
        els.push(Element::gesture_button(
            ids::OFFLINE_RECEIVE_BUTTON,
            fs_strings::OFFLINE_SHARE_RECEIVE,
            true,
            Gesture::Settings(Action::OpenOfflineReceive),
        ));
        return els;
    }

    // A panel is open: the compare pair, the act, the status, the way out.
    els.push(
        Element::label(ids::OFFLINE_SHARE_OWN_CODE, view.own_code.clone())
            .labelled(fs_strings::OFFLINE_SHARE_OWN_CODE_LABEL),
    );
    // The safety sentence rides as chrome, beside the code it is about — this
    // is the one place a user learns that handing the code over IN PERSON is
    // the mechanism, that sending it is not, and that the addressing groups at
    // the end are part of it (`p2p.md` § Offline share initiation → contract
    // point 1, *The compare code carries the addressing*).
    els.push(Element::chrome(fs_strings::OFFLINE_SHARE_OWN_CODE_HELP));
    els.push(
        Element::input(
            ids::OFFLINE_SHARE_PEER_CODE_INPUT,
            view.peer_code.clone(),
            Field::Settings(SettingsField::OfflineSharePeerCodeInput),
        )
        .labelled(fs_strings::OFFLINE_SHARE_PEER_CODE_LABEL),
    );
    // Typing guidance, not an operation failure — so it rides as chrome beside
    // the input rather than on `error-message` (convention 2 reserves that for
    // what an action actually did). An empty box says nothing: the act button
    // is simply disabled, which is the honest signal there.
    if let Err(why) = view.peer_actor()
        && let Some(hint) = crate::offline_share::code_error_text(why)
    {
        els.push(Element::chrome(hint));
    }

    match view.panel {
        OfflineSharePanel::Initiate => els.push(Element::gesture_button(
            ids::OFFLINE_SHARE_BEGIN_BUTTON,
            fs_strings::OFFLINE_SHARE_BEGIN,
            view.can_begin(),
            Gesture::Settings(Action::BeginOfflineShare),
        )),
        OfflineSharePanel::Receive => els.push(Element::gesture_button(
            ids::OFFLINE_RECEIVE_EXPECT_BUTTON,
            fs_strings::OFFLINE_SHARE_EXPECT,
            view.can_expect(),
            Gesture::Settings(Action::ExpectOfflineShare),
        )),
        // `shows_entry_buttons()` already returned for the closed panel.
        OfflineSharePanel::Closed => {}
    }

    els.push(Element::label(
        ids::OFFLINE_SHARE_STATUS,
        crate::offline_share::status_text(view.status),
    ));
    if view.shows_cancel() {
        els.push(Element::gesture_button(
            ids::OFFLINE_SHARE_CANCEL_BUTTON,
            common::CANCEL,
            true,
            Gesture::Settings(Action::CancelOfflineShare),
        ));
    }
    els
}

/// The peer-transfer surface (`p2p.md` § Cross-user shared-set transfer —
/// row 58; the six ids user-approved 2026-08-18). Render-only — no gestures:
/// `share-serve-status` is rule-5 transparency (is this device serving, and
/// why not when it is not), and the `share-transfer-item` rows are the
/// latest pull pass's per-(set × peer) outcomes, `share-transfer-state`
/// rendering the transfer gate's refusal honestly ("limited by …", Dim-3).
///
/// Nothing renders before the glue created its cell this session: a
/// never-started plane has nothing honest to say. Paint reads the cell,
/// never I/O (convention 11) — the lock is held for the clone alone.
#[cfg(feature = "p2p-share")]
fn share_transfer_elements(cell: Option<&crate::share_glue::SharePlaneCell>) -> Vec<Element> {
    use crate::share_glue::{
        ServeStatus, outcome_name_text, outcome_progress_text, outcome_state_text,
        serve_status_text,
    };

    let Some(cell) = cell else {
        return Vec::new();
    };
    let (status, outcomes) = {
        let s = cell.lock().unwrap();
        (s.status, s.outcomes.clone())
    };
    let mut els = vec![
        Element::chrome(fs_strings::SHARE_TRANSFER_SECTION),
        Element::label(ids::SHARE_SERVE_STATUS, serve_status_text(status)),
    ];
    // The list is present when the plane is up or has activity (ui.yaml's
    // stated presence rule); a bare container marker, the
    // `events-view-toggle` idiom.
    if matches!(status, ServeStatus::Serving(_)) || !outcomes.is_empty() {
        els.push(Element::label(ids::SHARE_TRANSFER_LIST, String::new()));
        for (k, o) in outcomes.iter().enumerate() {
            els.push(Element::label(ids::SHARE_TRANSFER_ITEM, String::new()));
            els.push(
                Element::label(ids::SHARE_TRANSFER_NAME, outcome_name_text(o))
                    .within(ids::SHARE_TRANSFER_ITEM, k),
            );
            els.push(
                Element::label(ids::SHARE_TRANSFER_PROGRESS, outcome_progress_text(o))
                    .within(ids::SHARE_TRANSFER_ITEM, k),
            );
            els.push(
                Element::label(ids::SHARE_TRANSFER_STATE, outcome_state_text(o))
                    .within(ids::SHARE_TRANSFER_ITEM, k),
            );
        }
    }
    els
}

/// The page-level "Shared with you" knock list — one `folder-pending-share` per
/// staged stranger share, each carrying `folder-share-accept-button` and
/// `folder-share-decline-button` (`ui/folders.md:168`).
///
/// Both buttons address the durable inbox row by `inbox_id`, not by position, so
/// a list that shifts under a concurrent drain can never accept the wrong knock.
/// The children are `.within(ids::FOLDER_PENDING_SHARE, k)` — a scoped read gets
/// the right card, and an UNSCOPED `click("…-accept-button", index=0)` (what the
/// shared helper does) still resolves, since `Registry::matches` resolves an
/// empty scope to the whole frame.
fn pending_share_elements(pending: &[PendingShareView]) -> Vec<Element> {
    if pending.is_empty() {
        return Vec::new();
    }
    let mut els = vec![Element::chrome(t::SHARED_WITH_YOU)];
    for (k, p) in pending.iter().enumerate() {
        // `shared_by_display` is pre-computed by the shared crate (handle else
        // canonical short_id) — the one string every app renders. It is empty
        // only for a fully unstamped cross-nest share, the one genuinely
        // locale-dependent branch, which falls back to the i18n label.
        let who = if p.shared_by_display.is_empty() {
            common::UNKNOWN.to_string()
        } else {
            p.shared_by_display.clone()
        };
        els.push(Element::label(
            ids::FOLDER_PENDING_SHARE,
            t::shared_by(&who),
        ));
        els.push(
            Element::gesture_button(
                ids::FOLDER_SHARE_ACCEPT_BUTTON,
                common::ACCEPT,
                true,
                Gesture::Settings(Action::AcceptFolderShare(p.inbox_id)),
            )
            .within(ids::FOLDER_PENDING_SHARE, k),
        );
        els.push(
            Element::gesture_button(
                ids::FOLDER_SHARE_DECLINE_BUTTON,
                common::DECLINE,
                true,
                Gesture::Settings(Action::DeclineFolderShare(p.inbox_id)),
            )
            .within(ids::FOLDER_PENDING_SHARE, k),
        );
    }
    els
}

/// The co-present ceremony's consent cards — the same `folder-pending-share`
/// trio, over the ceremony record's `invited` side instead of the durable inbox
/// (`p2p.md` § Offline share initiation, contract point 1).
///
/// `first_index` is where this list continues the M2 knocks: the two sources
/// share ONE indexed family, so a scoped read addresses whichever card it
/// means, and a user is never asked to understand that two different transports
/// brought two different lists of the same question.
///
/// Both gestures carry the **scope id**, never the row index. The knock list's
/// own rule and for a sharper reason here: an offer can land while the user's
/// finger is moving — the listener ingests it without asking the page — so a
/// position-addressed Accept could genuinely consent to the wrong share.
#[cfg(feature = "p2p-share")]
fn group_invitation_elements(
    invitations: &[crate::offline_share::PendingGroupShareView],
    first_index: usize,
) -> Vec<Element> {
    let mut els = Vec::new();
    for (offset, inv) in invitations.iter().enumerate() {
        let k = first_index + offset;
        // The set is nameless in v1, so the card names the two things that ARE
        // known: who is handing it over, and the short scope id both people can
        // see on their own screens.
        els.push(Element::label(
            ids::FOLDER_PENDING_SHARE,
            fs_strings::offline_share_from(&inv.initiator, &inv.short_id),
        ));
        els.push(
            Element::gesture_button(
                ids::FOLDER_SHARE_ACCEPT_BUTTON,
                common::ACCEPT,
                true,
                Gesture::Settings(Action::AcceptGroupShare(inv.scope_id)),
            )
            .within(ids::FOLDER_PENDING_SHARE, k),
        );
        els.push(
            Element::gesture_button(
                ids::FOLDER_SHARE_DECLINE_BUTTON,
                common::DECLINE,
                true,
                Gesture::Settings(Action::DeclineGroupShare(inv.scope_id)),
            )
            .within(ids::FOLDER_PENDING_SHARE, k),
        );
    }
    els
}

/// One `folder-row` per shared set whose machinery this device actually holds,
/// continuing the M2 set list's index from `first_index`.
///
/// A row here is deliberately thin, and every absence is a fact rather than a
/// gap: no conflict-policy select (a group scope has no local scan seat yet),
/// no expander body (nothing to configure), no leave button (severance is the
/// authority's mint, not a self-scoped roster drop — `account-data-plane.md`
/// § The recipient-set scheme, the severance bullet). What it does carry is
/// the set's identity and its `folder-shared-badge`, in the same two readings
/// the M2 rows use: "Shared by ‹them›" on someone else's scope, "Shared · N"
/// on your own.
#[cfg(feature = "p2p-share")]
fn group_scope_row_elements(
    scopes: &[crate::offline_share::GroupScopeView],
    first_index: usize,
) -> Vec<Element> {
    let mut els = Vec::new();
    for (offset, scope) in scopes.iter().enumerate() {
        let i = first_index + offset;
        // Not a gesture: toggling expands a body this row does not have. A
        // button that visibly does nothing is worse than a label.
        els.push(Element::label(
            ids::FOLDER_ROW,
            fs_strings::offline_share_set(&scope.short_id),
        ));
        let badge = match &scope.shared_by {
            Some(who) => t::shared_by(who),
            None => t::shared_badge(&scope.member_count.to_string()),
        };
        els.push(Element::label(ids::FOLDER_SHARED_BADGE, badge).within(ids::FOLDER_ROW, i));
    }
    els
}

/// The owner-side "Shared with" section for the ONE expanded owner row
/// (`ui/folders.md` § Sharing — *Owner side*): the `folder-share-button` + its
/// inline share form, the `folder-shared-badge` "Shared · N", and one
/// `folder-member-item` per shared-with actor.
///
/// **The `role == "member"` filter is applied exactly once**, by the shared
/// `fauna_client_folders::member_actors` — the same call backs both the render
/// loop and the badge count (`.len()` of its result), which is what stops the
/// count and the list from ever disagreeing. Never re-filter `role` locally
/// (`ui/folders.md:175`).
///
/// tui has no modal dialogs, so linux's `adw::MessageDialog` share sheet becomes
/// The **"Folders you follow"** section — page-level, below the owner's own
/// rows (`ui/folders.md` § Following a public folder).
///
/// ⚠ Followed folders are a list SEPARATE from `folder-row`, and that separation
/// is the point rather than a layout choice: a followed folder has no group, no
/// roster, no local seat and no binding — a follower holds no keys and never
/// binds (`file-sync.md` § Multi-writer, the readers-never-bind rule) — and it
/// carries a *status* those rows cannot hold. Rendering one as a `folder-row`
/// would offer an expander full of controls that cannot apply to it.
///
/// The follow form REUSES `recipient-picker-input` for the handle half, never
/// re-minting a picker (priority #2, exactly as the share flow does).
fn followed_elements(
    followed: &[fauna_devices_machine::FollowedFolderSummary],
    ui: &FoldersUiState,
) -> Vec<Element> {
    let mut els = vec![Element::chrome(t::FOLLOWED_FOLDERS_SECTION)];

    els.push(Element::gesture_button(
        ids::FOLDER_FOLLOW_BUTTON,
        t::FOLLOW_PUBLIC_FOLDER,
        true,
        Gesture::Settings(Action::OpenFolderFollowForm),
    ));
    els.push(Element::chrome(t::FOLLOW_PUBLIC_FOLDER_HINT));

    if ui.follow_open {
        els.push(
            Element::input(
                ids::RECIPIENT_PICKER_INPUT,
                ui.follow_handle_input.clone(),
                Field::Settings(SettingsField::FolderFollowHandle),
            )
            .labelled(fauna_i18n::strings::conversations::unified::RECIPIENT_PICKER_PLACEHOLDER),
        );
        els.push(
            Element::input(
                ids::FOLDER_FOLLOW_NAME_INPUT,
                ui.follow_name_input.clone(),
                Field::Settings(SettingsField::FolderFollowName),
            )
            .labelled(t::FOLLOW_FOLDER_NAME),
        );
        els.push(Element::chrome(t::FOLLOW_FOLDER_NAME_HINT));
        els.push(Element::gesture_button(
            ids::FOLDER_FOLLOW_CONFIRM,
            t::FOLLOW_CONFIRM,
            true,
            Gesture::Settings(Action::ConfirmFolderFollow),
        ));
    }

    for (k, f) in followed.iter().enumerate() {
        // The row itself is a view the children scope under, mirroring
        // `folder-row` — so `folder-followed-item[k] / folder-unfollow-button`
        // resolves per row. Its text says WHOSE folder it is as well as which
        // (name + owner handle + badge + status): the owner rides the row, not
        // a line of its own, so the one read every app offers carries it. The
        // owner string is the shared precomputed `owner_display` — the handle
        // while it still names the owner, else the id's short form — painted as
        // given, never re-derived here.
        els.push(Element::label(
            ids::FOLDER_FOLLOWED_ITEM,
            format!(
                "{} · {}",
                f.display_name,
                t::followed_owner(&f.owner_display)
            ),
        ));
        els.push(
            Element::label(
                ids::FOLDER_FOLLOWED_STATUS,
                if f.available {
                    t::FOLLOWED_STATUS_FOLLOWING
                } else {
                    t::FOLLOWED_STATUS_UNAVAILABLE
                },
            )
            .within(ids::FOLDER_FOLLOWED_ITEM, k),
        );
        // The provenance badge: this row came from somewhere public, which is
        // why it has no keys and no seat.
        els.push(Element::chrome(t::FOLLOWED_PUBLIC_BADGE).within(ids::FOLDER_FOLLOWED_ITEM, k));
        if !f.available {
            // ⚠ Names BOTH causes (unshared / removed) because the follower
            // genuinely cannot tell them apart — the home nest folds them — and
            // the difference does not change what the user can do about it.
            els.push(
                Element::chrome(t::FOLLOWED_UNAVAILABLE_HINT).within(ids::FOLDER_FOLLOWED_ITEM, k),
            );
        }
        els.push(
            Element::gesture_button(
                ids::FOLDER_UNFOLLOW_BUTTON,
                t::UNFOLLOW_FOLDER,
                true,
                Gesture::Settings(Action::UnfollowFolder(k)),
            )
            .within(ids::FOLDER_FOLLOWED_ITEM, k),
        );
    }

    els
}

/// inline elements gated on `share_open` — the same shape `folder-delete-confirm`
/// already uses for its destructive confirm. The picker is REUSED by id
/// (`recipient-picker-input`), never re-minted (`ui/folders.md:147`).
///
/// Returns elements carrying only their INNER scope
/// (`folder-member-item[j]` for a member's children); the caller re-scopes the
/// whole body under `folder-row[i]`, producing the
/// `folder-row[i] / folder-member-item[j]` path the shared helper's
/// `member_handle(j, row=i)` queries.
fn shared_with_elements(fs: &FolderSummary, i: usize, ui: &FoldersUiState) -> Vec<Element> {
    let mut els = vec![Element::chrome(t::SHARED_WITH)];

    // The inputs of the published-folder writer warning — the SAME reach test
    // `website_serve_hint` applies (`audience` public, or paywalled), fed the
    // NORMALIZED audience exactly as the audience select above is, so an
    // unparseable column can never claim the folder is world-readable.
    let audience =
        fauna_folders_machine::normalize_audience(&fs.audience, fs.mls_group_id.is_some());
    let paywalled = fs.web_paywall_tier.is_some();
    let published_warning = |access: &str| {
        fauna_folders_machine::writer_grant_reach(access, &audience, paywalled).map(|reach| {
            Element::label(
                ids::FOLDER_WRITER_PUBLISHED_WARNING,
                reach.resolve(fauna_i18n::strings::lookup),
            )
        })
    };

    // The roster belongs to this row only if it was read FOR this set — a stale
    // one from a previously expanded row must never paint here.
    let fresh = ui.roster_for.as_deref() == Some(fs.name.as_str());
    let members: Vec<&fauna_protocol::folders::FolderActorMember> = if fresh {
        fauna_client_folders::member_actors(&ui.members)
    } else {
        Vec::new()
    };

    els.push(Element::gesture_button(
        ids::FOLDER_SHARE_BUTTON,
        t::SHARE_BUTTON,
        true,
        Gesture::Settings(Action::OpenFolderShareForm),
    ));

    if ui.share_open {
        els.push(
            Element::input(
                ids::RECIPIENT_PICKER_INPUT,
                ui.share_recipient_input.clone(),
                Field::Settings(SettingsField::FolderShareRecipient),
            )
            // The REUSED picker keeps the picker's own prompt (linux mounts the
            // whole RecipientPicker widget here, placeholder included) — the
            // old "Share…" label was a button title that never said what to
            // type (copy-audit corpus, 2026-08-04).
            .labelled(fauna_i18n::strings::conversations::unified::RECIPIENT_PICKER_PLACEHOLDER),
        );
        // The share-time access grant, from the shared catalog — Reader default.
        // A share-time writer grant carries no cap by construction, so picking
        // Writer always shows the advisory warning (`ui/folders.md:152`); the
        // owner can set a cap on the member row afterwards.
        els.push(
            Element::select(
                ids::FOLDER_SHARE_ROLE_SELECT,
                ui.share_access.clone(),
                SelectTarget::FolderShareAccess,
                fauna_folders_machine::member_access_options()
                    .into_iter()
                    .map(|o| o.value)
                    .collect(),
            )
            .labelled(t::MEMBER_ACCESS),
        );
        if ui.share_access == "writer" {
            els.push(Element::label(
                ids::FOLDER_WRITER_UNCAPPED_WARNING,
                t::WRITER_UNCAPPED_WARNING,
            ));
        }
        // STATE-based, not event-based: a Writer picked on an already-published
        // folder says so here, and it stacks with the quota warning above —
        // they name different consequences of one grant (`ui/folders.md`
        // § Sharing).
        els.extend(published_warning(&ui.share_access));
        els.push(Element::gesture_button(
            ids::FOLDER_SHARE_CONFIRM,
            t::SHARE_BUTTON,
            true,
            Gesture::Settings(Action::ConfirmFolderShare),
        ));
    }

    // "Shared · N" — shown only once the set actually has members, so an
    // owner-only set renders no badge (the assertion
    // `test_folder_owner_side_sharing_affordances` makes, and the state a
    // successful remove must return the row to).
    if !members.is_empty() {
        els.push(Element::label(
            ids::FOLDER_SHARED_BADGE,
            t::shared_badge(&members.len().to_string()),
        ));
    } else if fresh {
        els.push(Element::chrome(t::NOT_SHARED_YET));
    }

    for (j, m) in members.iter().enumerate() {
        let cap_input = ui.member_cap_inputs.get(j).cloned().unwrap_or_default();
        // Absent role row = reader, the nest's own fail-safe default.
        let access = m.access.clone().unwrap_or_else(|| "reader".into());
        let mut item = vec![
            // The canonical handle-else-short_id rule, resolved by the shared
            // formatter — a local fallback here is what rendered raw 64-hex
            // actor ids on linux before the lift (`ui/folders.md:251`).
            Element::label(
                ids::FOLDER_MEMBER_HANDLE,
                fauna_core::format::account_display_label(Some(&m.handle), &m.actor_id),
            ),
            // "Pending" is a future optimistic state: the nest roster reports
            // only actors the share actually reached (it cannot observe an MLS
            // join), so every returned member reads "Active" (`folders.md:149`).
            Element::label(ids::FOLDER_MEMBER_STATUS, common::ACTIVE),
            Element::select(
                ids::FOLDER_MEMBER_ROLE_SELECT,
                access.clone(),
                SelectTarget::FolderMemberAccess { row: i, member: j },
                fauna_folders_machine::member_access_options()
                    .into_iter()
                    .map(|o| o.value)
                    .collect(),
            )
            .labelled(t::MEMBER_ACCESS),
            // A COMMITTING input: nothing else on the member row writes the cap,
            // and the shared helper's commit idiom is type-then-click
            // (`actions/backups.py::set_member_cap`). Blank = uncapped, as
            // ratified — never a silent server-side default (`folders.md:152`).
            Element::input_commit(
                ids::FOLDER_MEMBER_CAP_INPUT,
                cap_input.clone(),
                Field::Settings(SettingsField::FolderMemberCap { member: j }),
                Gesture::Settings(Action::SetFolderMemberAccess {
                    row: i,
                    member: j,
                    access: None,
                }),
            )
            .labelled(t::MEMBER_BYTE_CAP),
        ];
        // Visible iff writer AND no cap — the advisory an uncapped writer can
        // spend the owner's entire quota (`ui/folders.md:152`). Read from the
        // BUFFER, not `m.byte_cap`, so it clears the moment the owner commits a
        // cap rather than only after the roster round-trip returns.
        if access == "writer" && fauna_core::format::parse_count_i64(&cap_input).is_none() {
            item.push(Element::label(
                ids::FOLDER_WRITER_UNCAPPED_WARNING,
                t::WRITER_UNCAPPED_WARNING,
            ));
        }
        // Independent of the cap — a byte cap bounds the owner's quota, not what
        // the writer can change for the people outside the set — and stacked
        // with the warning above rather than replacing it.
        item.extend(published_warning(&access));
        item.push(Element::gesture_button(
            ids::FOLDER_MEMBER_REMOVE_BUTTON,
            t::REMOVE_MEMBER,
            // No channel id ⇒ the set cannot be addressed for an evict; render
            // the button inert rather than let a click fail (linux's own
            // `set_sensitive(false)` arm).
            ui.roster_channel_id.is_some(),
            Gesture::Settings(Action::RemoveFolderMember { row: i, member: j }),
        ));
        els.push(Element::label(
            ids::FOLDER_MEMBER_ITEM,
            fauna_core::format::account_display_label(Some(&m.handle), &m.actor_id),
        ));
        els.extend(
            item.into_iter()
                .map(|e| e.within(ids::FOLDER_MEMBER_ITEM, j)),
        );
    }
    els
}

/// The expanded owner row's device-activity section — `folder-device-activity-item`
/// (indexed) plus its `-label`/`-count` children, one item per device with
/// recorded sync activity on this set (`fauna.folders.devices`). Distinct
/// from [`shared_with_elements`]'s cross-USER roster: this is the *device*
/// activity signal, the ordinary sync "who has pushed a change and how
/// many" read (ui.yaml's `folder-device-activity-item` description).
///
/// Guarded by [`FoldersUiState::device_activity_for`] exactly like
/// [`shared_with_elements`] guards on [`FoldersUiState::roster_for`] — a read
/// for a DIFFERENT set (a stale previous row, or a read still in flight) must
/// never paint here, and the empty-state chrome only renders once a read for
/// THIS set has actually landed (never while `!fresh`, which would otherwise
/// flash "no activity" before the async load resolves).
///
/// Returns elements carrying only their INNER scope
/// (`folder-device-activity-item[k]` for a device's children); the caller
/// re-scopes the whole body under `folder-row[i]`, producing the
/// `folder-row[i] / folder-device-activity-item[k]` path the shared helper's
/// `device_activity_change_count(index=k)` queries (unscoped counts/indices
/// resolve fine since only one row is ever expanded at a time).
fn device_activity_elements(fs: &FolderSummary, ui: &FoldersUiState) -> Vec<Element> {
    let mut els = vec![Element::chrome(t::DEVICE_ACTIVITY)];

    let fresh = ui.device_activity_for.as_deref() == Some(fs.name.as_str());
    let devices: &[fauna_protocol::folders::FolderDevice] =
        if fresh { &ui.device_activity } else { &[] };

    if devices.is_empty() {
        if fresh {
            els.push(Element::chrome(t::NO_DEVICE_ACTIVITY));
        }
    } else {
        for (k, d) in devices.iter().enumerate() {
            els.push(Element::label(
                ids::FOLDER_DEVICE_ACTIVITY_ITEM,
                d.label.clone(),
            ));
            let item = vec![
                Element::label(ids::FOLDER_DEVICE_ACTIVITY_LABEL, d.label.clone()),
                Element::label(
                    ids::FOLDER_DEVICE_ACTIVITY_COUNT,
                    d.change_count.to_string(),
                ),
            ];
            els.extend(
                item.into_iter()
                    .map(|e| e.within(ids::FOLDER_DEVICE_ACTIVITY_ITEM, k)),
            );
        }
    }
    els
}

/// The device-local folder↔set binding section nested under an expanded
/// `folder-row` (A6 Slice 3; `tui.md` § the sync-agent Folders paragraph,
/// `sync-agent.md` § Control plane split → the device-local folder binding). The
/// linux `location_binding.rs` twin: a `folder-location-list` anchor, one
/// `folder-location-row` per folder bound to *this* set (each with its
/// `folder-location-path` label + `folder-location-remove-button`, scoped `.within` the
/// row like every other tui item list — `atproto-app-credential-item`), then the
/// typed-path add form (`folder-location-path-input` + `folder-location-add-button`). The
/// set is **contextual** — the enclosing row names it, so there is no
/// `folder-location-fileset`/`-input`; and no `folder-location-browse-button` (an OS-picker
/// affordance tui declares absent — `tui.md` § Declared platform absences 4).
///
/// On a host whose agent hosts a placeholder surface (windows cfapi, linux
/// FUSE) each row also carries the per-binding `folder-location-mode-toggle`,
/// scoped within it — disabled, with the reason painted under it, where the
/// agent reports it cannot serve an on-demand binding.
///
/// A row whose set is holding deletes also carries the mass-delete floor's
/// confirm affordance (`folder-location-deletes-held` +
/// `folder-location-apply-deletes-button`), conditional on the hold and scoped
/// within the row like the remove button.
///
/// `locations` is the whole rendered set (empty before the sync agent installs / on
/// non-unix); filtering by `fs.name` here — the set-name key the shared
/// `LocationBindingsModel` uses — is what makes each expanded row show only its own
/// folders. Remove is keyed by the set name (the model's only remove API), so it
/// targets this set's bindings, matching linux.
///
/// The elements come back with only their INNER scope (`folder-location-row[k]` for a
/// row's children); the caller re-scopes the whole body under `folder-row[i]`, so
/// the registry paths end up `folder-row[i] / folder-location-row[k]` — the nesting
/// the shared suite queries.
fn location_elements(
    fs: &FolderSummary,
    ui: &FoldersUiState,
    locations: &[RenderedLocationBinding],
) -> Vec<Element> {
    let mut els = vec![Element::label(
        ids::FOLDER_LOCATION_LIST,
        sp::SYNCED_LOCATIONS,
    )];
    for (k, binding) in locations.iter().filter(|b| b.folder == fs.name).enumerate() {
        // The row anchor carries the path as its text (so a scope-less
        // `get_text("folder-location-row")` still reads it), with the path repeated as
        // an addressable `folder-location-path` child scoped within the row — the
        // `atproto-app-credential-item` shape.
        els.push(Element::label(
            ids::FOLDER_LOCATION_ROW,
            binding.path.clone(),
        ));
        els.push(
            Element::label(ids::FOLDER_LOCATION_PATH, binding.path.clone())
                .within(ids::FOLDER_LOCATION_ROW, k),
        );
        els.push(
            Element::gesture_button(
                ids::FOLDER_LOCATION_REMOVE_BUTTON,
                common::REMOVE,
                true,
                Gesture::Settings(Action::RemoveLocation(fs.name.clone())),
            )
            .within(ids::FOLDER_LOCATION_ROW, k),
        );
        // The per-binding on-demand switch (`on-demand-files.md` § On-Demand
        // Files → *The choice is the user's*; § Linux FUSE binding). What it
        // shows is the shared rule's answer, projected by
        // `sync_agent::rendered_locations`: `None` where no switch is rendered
        // (macOS), else the agent's mode for this binding, disabled where the
        // host's agent cannot serve on-demand. Checked = on-demand; the `state`
        // attr is the uniform `always|on-demand` read; the gesture sets the
        // OTHER mode for this row's path, and the row repaints from the agent's
        // answer, never from the keystroke.
        if let Some(toggle) = binding.mode {
            let (mode, other) = if toggle.on_demand {
                ("on-demand", "always")
            } else {
                ("always", "on-demand")
            };
            els.push(
                Element::checkbox_gesture(
                    ids::FOLDER_LOCATION_MODE_TOGGLE,
                    t::sync_locations::ON_DEMAND_LABEL,
                    toggle.on_demand,
                    Gesture::Settings(Action::SetLocationMode {
                        path: binding.path.clone(),
                        mode: other.to_string(),
                    }),
                )
                .enabled(toggle.enabled)
                .attr("state", mode)
                .within(ids::FOLDER_LOCATION_ROW, k),
            );
            // The reason beside the switch — the host has no mount helper, or
            // this location's own mount was refused. Untagged text, as on
            // linux (ui.yaml gives the line no id).
            if let Some(line) = toggle.notice.and_then(fauna_i18n::strings::lookup) {
                els.push(Element::chrome(line));
            }
        }
        // The mass-delete floor's confirm affordance, on the rows whose set is
        // holding (`delete-propagation.md` § A wholesale-vanished folder is
        // infrastructure failure). Every tracked file vanished at once, so the
        // engine recorded NOTHING — the nest still holds the set, and the line
        // says so before offering to change that.
        //
        // Rendered only while the hold stands: `0` is the reading that retracts
        // it, and a zeroed line or a disabled button would keep an offer to
        // destroy files standing over a folder that is perfectly healthy.
        if binding.deletes_held > 0 {
            let count = binding.deletes_held.to_string();
            els.push(
                Element::label(
                    ids::FOLDER_LOCATION_DELETES_HELD,
                    fs_strings::deletes_held(&count),
                )
                .within(ids::FOLDER_LOCATION_ROW, k),
            );
            els.push(
                Element::gesture_button(
                    ids::FOLDER_LOCATION_APPLY_DELETES_BUTTON,
                    fs_strings::apply_deletes(&count),
                    true,
                    // The SET, never the count above: the agent re-derives what
                    // is actually missing at click time, so a confirm racing a
                    // remount deletes nothing.
                    Gesture::Settings(Action::ApplyHeldDeletes(fs.name.clone())),
                )
                .within(ids::FOLDER_LOCATION_ROW, k),
            );
        }
        // The delete rail's unreadable-path line (`delete-propagation.md`
        // § Unreadable is not absent): part of the folder could not be read, so
        // nothing was changed and that subtree stopped syncing. Deliberately a
        // bare line — there is nothing to confirm, and an apply verb here would
        // be the bug; the remedy (permissions, the mount) is outside the app.
        if binding.deletes_skipped_unreadable > 0 {
            let count = binding.deletes_skipped_unreadable.to_string();
            els.push(
                Element::label(
                    ids::FOLDER_LOCATION_UNREADABLE,
                    fs_strings::unreadable(&count),
                )
                .within(ids::FOLDER_LOCATION_ROW, k),
            );
        }
    }
    els.push(
        Element::input(
            ids::FOLDER_LOCATION_PATH_INPUT,
            ui.location_path_input.clone(),
            Field::Settings(SettingsField::LocationPath),
        )
        .labelled(sp::LOCATION_PATH),
    );
    els.push(Element::gesture_button(
        ids::FOLDER_LOCATION_ADD_BUTTON,
        sp::ADD_LOCATION,
        true,
        Gesture::Settings(Action::AddLocation),
    ));
    els
}

/// One conflict review row — `conflict-type-badge` / `conflict-file-info` always,
/// `conflict-resolve-button` only for an auto-resolved row that retains a
/// non-winning candidate (`folders.md` § Conflicts). Mirrors linux
/// `conflicts.rs::build_conflict_row` exactly (same shared
/// `conflict_badge_label` + the same `(winning_manifest_hash, has_other_version)`
/// match).
fn conflict_elements(c: &ConflictSummary) -> Vec<Element> {
    let mut els = vec![
        Element::label(
            ids::CONFLICT_TYPE_BADGE,
            conflict_badge_label(c.resolution.as_deref(), c.resolved_at, &c.conflict_type)
                .resolve(fauna_i18n::strings::lookup),
        ),
        Element::label(ids::CONFLICT_FILE_INFO, c.file_info.clone()),
    ];
    if c.winning_manifest_hash.is_some() && c.has_other_version {
        els.push(Element::gesture_button(
            ids::CONFLICT_RESOLVE_BUTTON,
            t::conflicts::USE_OTHER_VERSION,
            true,
            Gesture::Settings(Action::UseOtherVersion(c.id)),
        ));
    }
    els
}

/// The create wizard's elements for its current step. Mirrors linux
/// `wizard.rs::render` (`back` hidden on `Name`; `next` shown through
/// `Devices`, `create` only on `Review`).
fn wizard_elements(wizard: &FolderWizardSnapshot) -> Vec<Element> {
    let mut els = Vec::new();
    match wizard.step {
        FolderWizardStep::Name => els.extend(name_elements(wizard)),
        FolderWizardStep::Devices => els.extend(device_places_elements(wizard)),
        FolderWizardStep::Review => els.extend(review_elements(wizard)),
        // Terminal — the client closes the wizard on `Done` before this can ever
        // paint (`Op::run`'s `WizardCreate` arm); reachable only for the one tick
        // between `submit()` returning and the fold closing it, which never
        // renders. Paint nothing rather than stale step-4 content.
        FolderWizardStep::Done => {}
    }
    if wizard.step != FolderWizardStep::Name {
        els.push(Element::gesture_button(
            ids::WIZARD_BACK_BUTTON,
            t::wizard::BACK,
            true,
            Gesture::Settings(Action::WizardBack),
        ));
    }
    if wizard.step == FolderWizardStep::Review {
        els.push(Element::gesture_button(
            ids::WIZARD_CREATE_BUTTON,
            t::wizard::CREATE,
            wizard.review.create_enabled,
            Gesture::Settings(Action::WizardCreate),
        ));
    } else {
        let continue_enabled = match wizard.step {
            FolderWizardStep::Name => wizard.name.continue_enabled,
            FolderWizardStep::Devices => wizard.device_places.continue_enabled,
            FolderWizardStep::Review | FolderWizardStep::Done => false,
        };
        els.push(Element::gesture_button(
            ids::WIZARD_NEXT_BUTTON,
            t::wizard::NEXT,
            continue_enabled,
            Gesture::Settings(Action::WizardNext),
        ));
    }
    els
}

/// Step 1 — the folder's name, and nothing else. The mode radios
/// (`wizard-mode-sync` / `-backup` / `-web`) went with the mode itself in
/// phase 2 slice e: a folder has no type, so there is no type step
/// (`folders.md` § Target re-model; plan doc § UX).
fn name_elements(wizard: &FolderWizardSnapshot) -> Vec<Element> {
    let nm = &wizard.name;
    let mut els = vec![
        Element::input(
            ids::WIZARD_NAME_INPUT,
            nm.name.clone(),
            Field::Settings(SettingsField::FolderWizardName),
        )
        .labelled(t::wizard::NAME_LABEL),
    ];
    // The disabled-Next explainer: `continue_enabled` gates on a non-empty
    // name (folders.md § Create wizard step 1), and a greyed-out Next with
    // zero on-screen signal why was a live-user "unknowable" report. One
    // line, only while the gate is actually closed.
    if !nm.continue_enabled {
        els.push(Element::chrome(t::wizard::NAME_REQUIRED));
    }
    els
}

/// Step 2 — enrollment + each seat's three place flags. Replaced the
/// Source/Sync/Backup/Mirror picker, which a live user called "a completely
/// incomprehensible list of things" (2026-08-05); the design's answer is plain
/// checkboxes that each say what they do, not better role nouns.
fn device_places_elements(wizard: &FolderWizardSnapshot) -> Vec<Element> {
    let dp = &wizard.device_places;
    // The step header linux paints (`select_devices_roles`) — without it the
    // step is a bare checkbox stack that never says what enrolling a device
    // *does* (copy-audit corpus, 2026-08-04).
    let mut els = vec![Element::chrome(t::wizard::SELECT_DEVICES_ROLES)];
    for (i, device) in dp.devices.iter().enumerate() {
        els.push(Element::checkbox_gesture(
            ids::WIZARD_DEVICE_CHECK,
            device.label.clone(),
            device.selected,
            Gesture::Settings(Action::ToggleWizardDeviceMember(i as u32)),
        ));
        for (id, label, desc, on, flag) in [
            (
                "wizard-device-originates",
                t::wizard::PLACE_ORIGINATES,
                t::wizard::PLACE_ORIGINATES_DESC,
                device.originates,
                PlaceFlag::Originates,
            ),
            (
                "wizard-device-accepts",
                t::wizard::PLACE_ACCEPTS,
                t::wizard::PLACE_ACCEPTS_DESC,
                device.accepts,
                PlaceFlag::Accepts,
            ),
            (
                "wizard-device-applies-deletes",
                t::wizard::PLACE_APPLIES_DELETES,
                t::wizard::PLACE_APPLIES_DELETES_DESC,
                device.applies_deletes,
                PlaceFlag::AppliesDeletes,
            ),
        ] {
            els.push(
                Element::checkbox_gesture(
                    id,
                    label,
                    on,
                    Gesture::Settings(Action::ToggleWizardDeviceFlag { row: i, flag }),
                )
                // Drivers read the tick through `get_attr(.., "state")` — the
                // same contract `folder-webdav-toggle` carries — so a shared
                // action can set a flag idempotently instead of blind-clicking.
                .attr("state", if on { "on" } else { "off" }),
            );
            // Each box carries its own one-line explainer, the same pattern the
            // retired role picker used for `role_*_desc` — the flags are the
            // thing being explained now, so the explainer is per box, not per
            // seat.
            els.push(Element::chrome(desc));
        }
    }
    els
}

/// The read-only Review-step summary. ui.yaml gives this step no element IDs
/// of its own (only the nav buttons + `wizard-create-button` are registered) —
/// every field here is untagged chrome, like the review page every other
/// app's wizard paints (a human-only summary; the agent's own assertions
/// read the create-time inputs it drove, not a re-derived review render).
fn review_elements(wizard: &FolderWizardSnapshot) -> Vec<Element> {
    let r = &wizard.review;
    // No mode line, no retention line, no cadence line: a folder has no
    // type, retention is the nest place's policy edited on the row (phase 2
    // slice e), and the scan cadence is a constant, not a choice (phase 5).
    let mut els = vec![Element::chrome(format!(
        "{}: {}",
        t::wizard::REVIEW_NAME,
        r.name
    ))];
    // While `submit()` is in flight the Create button is DIM with no other
    // signal — a disabled control owes a visible reason (vocabulary rule 3),
    // and this line is it.
    if r.phase == SubmitPhase::Submitting {
        els.push(Element::chrome(t::wizard::CREATING_FOLDER));
    }
    if let Some(error) = &r.error {
        els.push(Element::chrome(error.resolve(fauna_i18n::strings::lookup)));
    }
    els
}

/// Pre-fill [`FoldersUiState::include_paths_input`] /
/// `exclude_paths_input` from a row's stored paths — called once when
/// [`Action::ToggleFolderRow`] expands it, mirroring linux's `Entry::text(...)`
/// construction (`join_paths_field`, the shared inverse of `parse_paths_field`).
pub(super) fn prefill_paths(ui: &mut FoldersUiState, fs: Option<&FolderSummary>) {
    ui.include_paths_input = join_paths_field(fs.and_then(|fs| fs.include_paths.as_deref()));
    ui.exclude_paths_input = join_paths_field(fs.and_then(|fs| fs.exclude_paths.as_deref()));
    prefill_nest_place(ui, fs);
}

/// Seed the nest-place editor's four buffers from a row.
///
/// Split out from [`prefill_paths`] because it runs in one more place: a
/// refreshed `DevicesSnapshot` re-seeds it (through
/// [`reseed_nest_place_if_untouched`]), so the controls show what the nest
/// actually holds rather than what was last typed and saved. The path buffers
/// deliberately do NOT ride along there — see the call site.
pub(super) fn prefill_nest_place(ui: &mut FoldersUiState, fs: Option<&FolderSummary>) {
    // Every prefill rule — unset renders blank, a zero bound renders blank
    // because zero is the nest's own spelling of unset — lives in shared Rust,
    // so this is a transcription into the page's buffers and nothing else.
    let edit = nest_place_edit_from_row(
        fs.and_then(|fs| fs.nest_snapshots),
        fs.and_then(|fs| fs.nest_snapshot_quiet_secs),
        fs.and_then(|fs| fs.retention_policy.clone()),
    );
    ui.nest_snapshots_input = edit.snapshots;
    ui.nest_quiet_input = edit.quiet_secs;
    ui.nest_retention_snapshots_input = edit.retention_snapshots;
    ui.nest_retention_days_input = edit.retention_days;
    let vr = fauna_folders_machine::version_retention_edit_from_bounds(
        fs.map_or(0, |fs| fs.version_retention_max_versions),
        fs.map_or(0, |fs| fs.version_retention_max_age_days),
    );
    ui.version_retention_count_input = vr.count;
    ui.version_retention_days_input = vr.days;
    ui.nest_place_seeded = Some(nest_place_buffers(ui));
}

/// The six nest-place buffers, in one comparable value.
fn nest_place_buffers(ui: &FoldersUiState) -> [String; 6] {
    [
        ui.nest_snapshots_input.clone(),
        ui.nest_quiet_input.clone(),
        ui.nest_retention_snapshots_input.clone(),
        ui.nest_retention_days_input.clone(),
        ui.version_retention_count_input.clone(),
        ui.version_retention_days_input.clone(),
    ]
}

/// A refreshed snapshot's re-seed of the nest-place editor — skipped while the
/// user has staged edits there that `folder-nest-save-button` has not sent.
///
/// The page re-hydrates on every account-store change, and a busy account
/// changes its store all the time: an unconditional re-seed wiped staged values
/// back to the nest's between the typing and the save, and the save then sent
/// the old policy with no error. So a refresh moves the boxes only while they
/// still hold what was last seeded (nobody has typed), or once the save has
/// released them — which is how the save's own refresh shows what the nest
/// actually stored rather than what was typed.
pub(super) fn reseed_nest_place_if_untouched(ui: &mut FoldersUiState, fs: Option<&FolderSummary>) {
    let untouched = match &ui.nest_place_seeded {
        Some(seeded) => *seeded == nest_place_buffers(ui),
        None => true,
    };
    if untouched {
        prefill_nest_place(ui, fs);
    }
}

/// `folder-nest-save-button` sent the buffers: they are no longer the user's,
/// so the next refresh re-seeds them from what the nest stored.
pub(super) fn release_nest_place(ui: &mut FoldersUiState) {
    ui.nest_place_seeded = None;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::element::Role;
    use crate::sync_agent::RenderedModeToggle;
    use fauna_folders_machine::{
        DevicePlacesSnapshot, NEST_SNAPSHOTS_DEFAULT, NameSnapshot, RetentionPolicy,
        ReviewSnapshot, SubmitPhase, WizardDevice, retention_from_inputs,
    };

    fn empty_state() -> SettingsState {
        SettingsState::default()
    }

    fn snapshot_with_folders(
        folders: Vec<FolderSummary>,
    ) -> fauna_devices_machine::DevicesSnapshot {
        fauna_devices_machine::DevicesSnapshot {
            folders,
            ..Default::default()
        }
    }

    /// Struct-update form on purpose: `FolderSummary` grows on a wire-additive
    /// cadence, and a hand-listed literal breaks every fixture on each growth
    /// (the house convention documented on the type's `Default`).
    fn folder(name: &str) -> FolderSummary {
        FolderSummary {
            id: 1,
            name: name.to_string(),
            ..Default::default()
        }
    }

    /// A folder with its website toggle ON — what the paywall row keys on
    /// (never the retired `mode = "web"` spelling).
    fn website_folder(name: &str) -> FolderSummary {
        let mut fs = folder(name);
        fs.website_enabled = true;
        fs
    }

    fn el<'a>(els: &'a [Element], id: &str) -> Option<&'a Element> {
        els.iter().find(|e| e.id == id)
    }

    /// A `Role::Select`'s offered option values (there is no accessor — every
    /// caller destructures the role, `admin/web.rs`' idiom).
    fn options(el: &Element) -> Vec<String> {
        match &el.role {
            crate::element::Role::Select { options, .. } => options.clone(),
            other => panic!("not a select: {other:?}"),
        }
    }

    #[test]
    fn empty_list_paints_only_heading_sync_defaults_add_button_and_nav_back() {
        let state = empty_state();
        let els = folders_elements(&state, &[]);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "page-heading",
                // the untagged "Sync defaults" group title (chrome — ui.yaml
                // gives the section no id, and inventing one is the anti-pattern)
                "",
                "sync-default-conflict-policy-select",
                "folder-add-button",
                // The "Folders you follow" section renders unconditionally: the
                // follow button is how a user gets their FIRST followed folder,
                // so gating the section on already having one would make it
                // unreachable. The two blanks are its title and hint chrome.
                "",
                "folder-follow-button",
                "",
                "settings-nav-back",
            ]
        );
    }

    /// The page-level default renders unconditionally, and an unrecorded
    /// preference (`None`) shows `auto` — the policy a new set would actually
    /// get, so there is no "unknown" state for the user to interpret.
    #[test]
    fn sync_default_conflict_policy_select_falls_back_to_auto_when_unset() {
        let state = empty_state();
        assert!(state.folders.default_conflict_policy.is_none());
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "sync-default-conflict-policy-select").expect("select painted");
        assert_eq!(sel.text, "auto");
    }

    /// A loaded preference is what the select shows — the "render what was
    /// persisted, never what was typed" rule the save path also honours by
    /// folding the NORMALIZED stored value back through the same field.
    #[test]
    fn sync_default_conflict_policy_select_shows_the_loaded_preference() {
        let mut state = empty_state();
        state.folders.default_conflict_policy = Some("latest_wins_always".into());
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "sync-default-conflict-policy-select").expect("select painted");
        assert_eq!(sel.text, "latest_wins_always");
    }

    // ── Following a public folder (phase 4 slice 4f-iii) ──────────────────

    fn followed(name: &str, available: bool) -> fauna_devices_machine::FollowedFolderSummary {
        fauna_devices_machine::FollowedFolderSummary {
            folder_id: 1,
            home_nest_url: "https://other.example".into(),
            owner_actor_id: "ab".repeat(32),
            display_name: name.to_string(),
            available,
            ..Default::default()
        }
    }

    /// The followed row says whose folder it is — *name + owner handle + badge +
    /// status* (`ui/folders.md` § Following a public folder) — and the owner rides
    /// the ROW's own text, so the one scoped read every app can make
    /// (`folder-followed-item[k]`) carries it, as it carries the name. The string
    /// is the shared precomputed `owner_display`, painted as given: tui never
    /// re-derives the handle-or-short-id fallback.
    #[test]
    fn a_followed_row_says_whose_folder_it_is() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_followed(vec![
            fauna_devices_machine::FollowedFolderSummary {
                owner_handle: Some("alice".into()),
                owner_display: "alice".into(),
                ..followed("their-site", true)
            },
        ]));
        let els = folders_elements(&state, &[]);
        let row = el(&els, "folder-followed-item").expect("row painted");
        assert!(row.text.contains("their-site"), "the name: {:?}", row.text);
        assert!(
            row.text.contains(&t::followed_owner("alice")),
            "the owner: {:?}",
            row.text
        );
    }

    fn snapshot_with_followed(
        followed: Vec<fauna_devices_machine::FollowedFolderSummary>,
    ) -> fauna_devices_machine::DevicesSnapshot {
        fauna_devices_machine::DevicesSnapshot {
            followed,
            ..Default::default()
        }
    }

    /// The follow form is ARMED, not always painted — but the button that arms
    /// it always is, because it is how a user gets their first followed folder.
    #[test]
    fn the_follow_form_is_armed_while_its_button_always_shows() {
        let mut state = empty_state();
        assert!(
            el(&folders_elements(&state, &[]), "folder-follow-button").is_some(),
            "the entry point must not be gated on already following something"
        );
        assert!(el(&folders_elements(&state, &[]), "folder-follow-name-input").is_none());
        assert!(el(&folders_elements(&state, &[]), "folder-follow-confirm").is_none());

        state.folders.follow_open = true;
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-follow-name-input").is_some());
        assert!(el(&els, "folder-follow-confirm").is_some());
        // ⚠ The handle half REUSES the shared picker rather than minting a
        // second one (priority #2, as the share flow does). If this regresses
        // into a new id, the cross-app picker contract quietly forks.
        assert!(
            el(&els, "recipient-picker-input").is_some(),
            "the follow flow must reuse recipient-picker-input for the handle"
        );
    }

    /// A followed folder is a row in its OWN list, never a `folder-row`: it has
    /// no roster, no binding and no seat, so rendering it as one would offer an
    /// expander full of controls that cannot apply to it.
    #[test]
    fn a_followed_folder_is_not_a_folder_row() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_followed(vec![followed("their-site", true)]));
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-followed-item").is_some());
        assert!(
            el(&els, "folder-row").is_none(),
            "a followed folder must not paint as one of the user's own rows"
        );
    }

    /// The status is the verdict, and the unavailable row keeps its unfollow
    /// affordance — the revoke does not remove the row, the user does.
    #[test]
    fn the_followed_status_paints_the_verdict_and_the_row_survives_a_revoke() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_followed(vec![
            followed("live-one", true),
            followed("revoked-one", false),
        ]));
        let els = folders_elements(&state, &[]);

        let statuses: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "folder-followed-status")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(
            statuses,
            [t::FOLLOWED_STATUS_FOLLOWING, t::FOLLOWED_STATUS_UNAVAILABLE]
        );

        // Both rows keep an unfollow button — removal is the user's call.
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "folder-unfollow-button")
                .count(),
            2
        );
        // And the revoked one explains itself rather than just going grey.
        assert!(
            els.iter().any(|e| e.text == t::FOLLOWED_UNAVAILABLE_HINT),
            "an unavailable row must say what happened"
        );
    }

    /// Every child scopes under its own `folder-followed-item[k]`, or a scoped
    /// read resolves nothing — the same containment rule the folder rows follow.
    #[test]
    fn followed_row_children_scope_under_their_own_row() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_followed(vec![
            followed("first", true),
            followed("second", false),
        ]));
        let els = folders_elements(&state, &[]);
        for (id, k) in [
            ("folder-followed-status", 0usize),
            ("folder-unfollow-button", 0),
        ] {
            let e = els
                .iter()
                .filter(|e| e.id == id)
                .nth(k)
                .unwrap_or_else(|| panic!("{id}[{k}] painted"));
            assert_eq!(e.path, vec![("folder-followed-item".to_string(), k)]);
        }
        // The second row's children carry index 1, not 0.
        let second = els
            .iter()
            .filter(|e| e.id == "folder-unfollow-button")
            .nth(1)
            .expect("second unfollow");
        assert_eq!(second.path, vec![("folder-followed-item".to_string(), 1)]);
    }

    /// Audience + website render on EVERY expanded owner row — a folder has no
    /// type to gate them on, and gating the website toggle would re-create the
    /// website-creation gap (`folders.md` § Implementation status today).
    #[test]
    fn audience_and_website_paint_on_any_expanded_row() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![
            folder("photos"),
            folder("archive"),
        ]));
        // Collapsed: the body is not rendered, so neither control exists.
        assert!(el(&folders_elements(&state, &[]), "folder-audience-select").is_none());
        assert!(el(&folders_elements(&state, &[]), "folder-website-toggle").is_none());

        for row in [0usize, 1] {
            state.folders.expanded = Some(row);
            let els = folders_elements(&state, &[]);
            let sel = el(&els, "folder-audience-select")
                .unwrap_or_else(|| panic!("row {row} must paint the audience select"));
            let toggle = el(&els, "folder-website-toggle")
                .unwrap_or_else(|| panic!("row {row} must paint the website toggle"));
            // Scoped under their own row, or a scoped read finds nothing.
            assert_eq!(sel.path, vec![("folder-row".to_string(), row)]);
            assert_eq!(toggle.path, vec![("folder-row".to_string(), row)]);
        }
    }

    /// An UNBOUND folder chooses between Private and Public. `shared` is absent
    /// because the nest refuses it while unbound — offering it would be a
    /// control that fails on click.
    #[test]
    fn audience_select_offers_private_and_public_while_unbound() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-audience-select").expect("painted");
        assert_eq!(
            options(sel),
            vec!["private".to_string(), "public".to_string()]
        );
    }

    /// A BOUND folder renders `shared` as its current state and may still go
    /// public; `private` is withheld because the nest refuses it while bound
    /// (the honest repair is to remove the sharing first).
    #[test]
    fn audience_select_renders_shared_for_a_bound_folder_and_withholds_private() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        let mut fs = folder("docs");
        fs.mls_group_id = Some("aabb".into());
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-audience-select").expect("painted");
        assert_eq!(
            options(sel),
            vec!["shared".to_string(), "public".to_string()]
        );
        assert_eq!(
            sel.text, "shared",
            "a bound folder must say what it IS, even though the value is not pickable"
        );
        assert!(
            els.iter()
                .any(|e| e.text == fauna_i18n::strings::devices::FOLDER_AUDIENCE_SHARED_HINT),
            "the bound hint explains why shared is not a destination here"
        );
    }

    /// a BOUND folder currently PUBLIC offers `shared` as the way
    /// back (the one legal exit from its public window; the pick re-seals the
    /// corpus for its members), and the hint says so instead of claiming
    /// `shared` is unreachable.
    #[test]
    fn audience_select_offers_shared_as_the_exit_for_a_bound_public_folder() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        let mut fs = folder("docs");
        fs.mls_group_id = Some("aabb".into());
        fs.audience = "public".to_string();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-audience-select").expect("painted");
        assert_eq!(
            options(sel),
            vec!["shared".to_string(), "public".to_string()]
        );
        assert_eq!(sel.text, "public", "the select paints the current audience");
        assert!(
            els.iter()
                .any(|e| e.text == fauna_i18n::strings::devices::FOLDER_AUDIENCE_PUBLIC_BOUND_HINT),
            "the hint must explain the flip-back, not claim shared is unreachable"
        );
    }

    /// ⚠ The fail-closed floor. An absent audience (sent as nothing),
    /// and an unparseable value must NEVER paint as Public —
    /// that would tell the user their folder is world-readable on the strength
    /// of a string this binary could not read.
    #[test]
    fn an_absent_audience_paints_private_never_public() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        let fs = folder("photos");
        assert_eq!(
            fs.audience, "",
            "the fixture is a folder summary with no audience"
        );
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-audience-select").expect("painted");
        assert_eq!(sel.text, "private");
        // And the value the select shows is one it actually offers.
        assert!(options(sel).contains(&sel.text));
    }

    /// The declassify confirm is ARMED, never painted by default — the flip to
    /// public rests the folder unsealed, names and paths included, so it carries
    /// an explicit owner confirm (`principles.md` § The user always controls
    /// their data owns that one exception).
    #[test]
    fn declassify_confirm_paints_only_once_armed() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("site")]));
        assert!(
            el(
                &folders_elements(&state, &[]),
                "folder-audience-public-confirm"
            )
            .is_none(),
            "unarmed rows must not offer the confirm"
        );
        let unarmed = folders_elements(&state, &[]);
        for id in [
            "folder-audience-public-names-warning",
            "folder-audience-public-reseal-warning",
        ] {
            assert!(el(&unarmed, id).is_none(), "{id} rides the confirm");
        }

        state.folders.audience_public_pending = true;
        let els = folders_elements(&state, &[]);
        let confirm = el(&els, "folder-audience-public-confirm").expect("armed ⇒ painted");
        assert!(confirm.enabled);
        assert_eq!(confirm.path, vec![("folder-row".to_string(), 0)]);
        // Both consequences paint by id, beside the confirm they explain.
        let names = el(&els, "folder-audience-public-names-warning").expect("armed ⇒ painted");
        assert_eq!(names.text, t::DECLASSIFY_BODY);
        assert_eq!(names.path, vec![("folder-row".to_string(), 0)]);
        let reseal = el(&els, "folder-audience-public-reseal-warning").expect("armed ⇒ painted");
        assert_eq!(reseal.text, t::DECLASSIFY_IRREVERSIBLE);
        // ⚠ The select still paints the CURRENT audience: the nest has not moved,
        // and showing `public` before the confirm would report an audience the
        // folder does not have.
        let sel = el(&els, "folder-audience-select").expect("painted");
        assert_eq!(sel.text, "private");
    }

    /// The owner's re-confirm door (`ui/folders.md` § Audience and website
    /// serving): a folder the nest reports public whose attestation does not
    /// verify under THIS seat's own actor id paints the status and a button
    /// that ARMS the existing declassify confirm — it never writes on its own.
    /// A genuinely attested public folder, a private one, and the WebDAV
    /// fail-safe state paint neither (the shared predicate owns that split).
    #[test]
    fn an_unattested_public_folder_offers_the_owner_a_reconfirm() {
        let own_secret = [0x3Cu8; 32];
        let own = fauna_core::identity::ActorKeypair::from_secret(own_secret);
        let mut state = empty_state();
        state.secret_hex = fauna_core::hex32::encode(&own_secret).into();
        state.folders.expanded = Some(0);

        let mut unattested = folder("site");
        unattested.audience = "public".into();
        state.devices.snapshot = Some(snapshot_with_folders(vec![unattested.clone()]));
        let els = folders_elements(&state, &[]);
        let status = el(&els, "folder-audience-unattested").expect("unverified ⇒ painted");
        assert_eq!(status.text, t::FOLDER_AUDIENCE_UNATTESTED);
        assert_eq!(status.path, vec![("folder-row".to_string(), 0)]);
        let button = el(&els, "folder-audience-reconfirm-button").expect("painted with it");
        assert!(button.enabled);
        assert_eq!(button.path, vec![("folder-row".to_string(), 0)]);
        assert!(
            matches!(
                button.gesture(),
                Some(Gesture::Settings(Action::SetFolderAudience { row: 0, ref audience }))
                    if audience == "public"
            ),
            "the button ARMS the declassify confirm (picking public), never commits"
        );
        assert!(
            el(&els, "folder-audience-public-confirm").is_none(),
            "nothing armed until the owner presses it"
        );

        // Armed: the dialog paints and the button steps aside for its confirm.
        state.folders.audience_public_pending = true;
        let armed = folders_elements(&state, &[]);
        assert!(el(&armed, "folder-audience-public-confirm").is_some());
        assert!(el(&armed, "folder-audience-reconfirm-button").is_none());
        state.folders.audience_public_pending = false;

        let mut attested = unattested.clone();
        attested.id = 7;
        attested.audience_attestation = Some(
            fauna_protocol::folders::AudienceAttestation::mint(&own, 7, "site", 1_000, None).into(),
        );
        let mut private = unattested.clone();
        private.audience = "private".into();
        let mut served = unattested.clone();
        served.webdav_enabled = true;
        for (why, fs) in [
            ("genuinely attested", attested),
            ("private", private),
            ("the WebDAV fail-safe state", served),
        ] {
            state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
            let els = folders_elements(&state, &[]);
            assert!(el(&els, "folder-audience-unattested").is_none(), "{why}");
            assert!(
                el(&els, "folder-audience-reconfirm-button").is_none(),
                "{why}"
            );
        }

        // A seat with no identity cannot judge — it paints nothing rather than
        // a claim it cannot back.
        state.secret_hex = String::new().into();
        state.devices.snapshot = Some(snapshot_with_folders(vec![unattested]));
        assert!(el(&folders_elements(&state, &[]), "folder-audience-unattested").is_none());
    }

    /// The residency select paints on the expanded OWNER row with the
    /// fail-closed reading of the wire value (empty = Full), offers exactly the
    /// two shared options in the shared order, and its confirm is ARMED, never
    /// painted by default — the flip to metadata-only deletes the nest's copy
    /// of the folder's content (`file-sync.md` § Content residency).
    #[test]
    fn residency_select_paints_fail_closed_and_its_confirm_only_once_armed() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("site")]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-nest-residency-select").expect("painted on an owner row");
        assert_eq!(sel.text, "full", "an empty wire value paints as Full");
        assert_eq!(
            options(sel),
            vec!["full".to_string(), "metadata_only".to_string()]
        );
        assert_eq!(sel.path, vec![("folder-row".to_string(), 0)]);
        assert!(
            el(&els, "folder-residency-confirm").is_none(),
            "unarmed rows must not offer the confirm"
        );

        state.folders.residency_pending = true;
        let els = folders_elements(&state, &[]);
        let confirm = el(&els, "folder-residency-confirm").expect("armed ⇒ painted");
        assert!(confirm.enabled);
        assert_eq!(confirm.path, vec![("folder-row".to_string(), 0)]);
        // ⚠ The select still paints the CURRENT residency while armed: the
        // nest has not moved, and showing metadata-only before the confirm
        // would claim the nest holds no copy while it still does.
        let sel = el(&els, "folder-nest-residency-select").expect("painted");
        assert_eq!(sel.text, "full");

        // A metadata-only row paints as such — the one value that may.
        let mut fs = folder("site");
        fs.residency = "metadata_only".to_string();
        state.folders.residency_pending = false;
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-nest-residency-select").expect("painted");
        assert_eq!(sel.text, "metadata_only");
    }

    /// The website toggle stays ENABLED while the folder is neither public nor
    /// paywalled — the setting is real and merely inert there, so it hints
    /// rather than disabling (a disabled control would imply unavailability).
    #[test]
    fn website_toggle_hints_rather_than_disabling_without_a_readable_audience() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("site")]));
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-website-toggle").expect("painted");
        assert!(
            toggle.enabled,
            "the toggle is a real setting, just not yet visible to anyone"
        );
        assert_eq!(
            toggle.label.as_deref(),
            Some(t::SERVE_WEBSITE_NEEDS_AUDIENCE)
        );

        // Public ⇒ the ordinary hint.
        let mut fs = folder("site");
        fs.audience = "public".into();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-website-toggle").expect("painted");
        assert_eq!(toggle.label.as_deref(), Some(t::SERVE_WEBSITE_HINT));
    }

    /// A paywalled folder is also readable-by-someone, so it gets the ordinary
    /// hint too — the site serves to subscribers.
    #[test]
    fn website_toggle_accepts_a_paywall_as_a_readable_audience() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        let mut fs = folder("site");
        fs.web_paywall_tier = Some("gold".into());
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-website-toggle").expect("painted");
        assert_eq!(toggle.label.as_deref(), Some(t::SERVE_WEBSITE_HINT));
    }

    /// The hint's tri-state on the LIVE web-address flag
    /// (`website_serve_hint`): a known-OFF address gets the pointed
    /// nobody-can-reach-it wording, a known-ON one the plain live wording —
    /// and the tests above pin the third arm (unknown ⇒ the combined hedge,
    /// which is exactly what their flag-less fixtures exercise).
    #[test]
    fn website_toggle_hint_keys_on_the_live_address_flag() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        let mut fs = folder("site");
        fs.audience = "public".into();

        let mut snap = snapshot_with_folders(vec![fs.clone()]);
        snap.website_address_enabled = Some(false);
        state.devices.snapshot = Some(snap);
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-website-toggle").expect("painted");
        assert_eq!(
            toggle.label.as_deref(),
            Some(t::SERVE_WEBSITE_ADDRESS_OFF),
            "published here + address off is the one misleading case — say it"
        );

        let mut snap = snapshot_with_folders(vec![fs]);
        snap.website_address_enabled = Some(true);
        state.devices.snapshot = Some(snap);
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-website-toggle").expect("painted");
        assert_eq!(toggle.label.as_deref(), Some(t::SERVE_WEBSITE_LIVE));
    }

    /// Both hints paint by id on the expanded owner row, scoped under it, with
    /// the shared resolutions: the audience hint rides `(bound, current)` (a
    /// shared folder says the sharing has to go first), the website hint is the
    /// same tri-state the toggle's `label` carries.
    #[test]
    fn audience_and_website_hints_paint_by_id() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("site")]));
        let els = folders_elements(&state, &[]);
        let hint = el(&els, "folder-audience-hint").expect("painted");
        assert_eq!(hint.text, t::FOLDER_AUDIENCE_HINT);
        assert_eq!(hint.path, vec![("folder-row".to_string(), 0)]);
        let website = el(&els, "folder-website-hint").expect("painted");
        assert_eq!(website.text, t::SERVE_WEBSITE_NEEDS_AUDIENCE);
        assert_eq!(website.path, vec![("folder-row".to_string(), 0)]);

        let mut fs = folder("site");
        fs.mls_group_id = Some("00".repeat(32));
        fs.audience = "public".into();
        let mut snap = snapshot_with_folders(vec![fs.clone()]);
        snap.website_address_enabled = Some(false);
        state.devices.snapshot = Some(snap);
        let els = folders_elements(&state, &[]);
        let hint = el(&els, "folder-audience-hint").expect("painted");
        assert_eq!(hint.text, t::FOLDER_AUDIENCE_PUBLIC_BOUND_HINT);
        let website = el(&els, "folder-website-hint").expect("painted");
        assert_eq!(website.text, t::SERVE_WEBSITE_ADDRESS_OFF);

        fs.audience = "shared".into();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let hint = el(&els, "folder-audience-hint").expect("painted");
        assert_eq!(hint.text, t::FOLDER_AUDIENCE_SHARED_HINT);
    }

    /// The toggle paints the STORED state, both ways — the `attr("state", ...)`
    /// the driver reads.
    #[test]
    fn website_toggle_paints_the_stored_state() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        let mut fs = folder("site");
        fs.website_enabled = true;
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-website-toggle").expect("painted");
        assert_eq!(toggle.attrs, vec![("state".to_string(), "on".to_string())]);
    }

    /// `folder-paywall-tier-select` keys on the WEBSITE TOGGLE, never on the
    /// retired `mode = "web"` spelling: the toggle is the only door to a
    /// website folder since the wizard's mode step retired, so a gate on the
    /// spelling left the picker unreachable on every folder a user can make
    /// (the nest's own paywall gate re-keyed in phase 4; the row followed
    /// with the mode contraction). Website ON → painted, scoped under its
    /// row; website OFF → absent, whatever the row's other columns say.
    #[test]
    fn paywall_select_keys_on_the_website_toggle() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        let off = folder("site");
        assert!(!off.website_enabled, "the fixture rests website-off");
        state.devices.snapshot = Some(snapshot_with_folders(vec![off]));
        assert!(
            el(&folders_elements(&state, &[]), "folder-paywall-tier-select").is_none(),
            "a website-off row must not offer a paywall"
        );

        state.devices.snapshot = Some(snapshot_with_folders(vec![website_folder("site")]));
        let els = folders_elements(&state, &[]);
        let select = el(&els, "folder-paywall-tier-select")
            .expect("a website-enabled row must paint the paywall select");
        assert_eq!(select.path, vec![("folder-row".to_string(), 0)]);
    }

    /// `folder-webdav-toggle` paints on EVERY owner row — a folder has no type,
    /// so the former sync-type gate retired with the mode (`webdav-server.md`
    /// § What the namespace is) — and lives in the EXPANDED body, scoped under
    /// its own row — the shape the shared helper reads
    /// (`actions/backups.py::toggle_webdav`, "on the expanded row").
    #[test]
    fn webdav_toggle_paints_on_every_expanded_owner_row() {
        let mut state = empty_state();
        state.folders.can_serve_webdav = true;
        state.devices.snapshot = Some(snapshot_with_folders(vec![
            folder("photos"),
            folder("archive"),
        ]));
        // Collapsed: no toggle at all (the body is not rendered).
        assert!(el(&folders_elements(&state, &[]), "folder-webdav-toggle").is_none());

        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-webdav-toggle").expect("an owner row paints the toggle");
        assert!(toggle.enabled, "an MSEK-holding actor may flip it");
        assert_eq!(
            toggle.path,
            vec![("folder-row".to_string(), 0)],
            "the toggle must be scoped under its own row, or a scoped read finds nothing"
        );

        // …and so does every other owner row — there is no second kind of folder.
        state.folders.expanded = Some(1);
        assert!(el(&folders_elements(&state, &[]), "folder-webdav-toggle").is_some());
    }

    /// ⚠ The capability gate, not a hint: an actor with no MSEK still SEES the
    /// toggle but cannot fire it. `serve_set` commits the nest's `webdav_enabled`
    /// flag before it provisions the keys blob, so an enabled-but-doomed toggle
    /// would leave the set served-but-blobless (`ui/folders.md:59`).
    #[test]
    fn webdav_toggle_is_disabled_with_a_mail_hint_without_an_msek() {
        let mut state = empty_state();
        state.folders.can_serve_webdav = false;
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "folder-webdav-toggle").expect("still painted");
        assert!(
            !toggle.enabled,
            "no MSEK ⇒ the toggle must not be actionable"
        );
        assert_eq!(toggle.label.as_deref(), Some(t::SERVE_WEBDAV_NEEDS_MAIL));
    }

    /// `folder-paywall-tier-select` is website-enabled-only — the structural sibling of
    /// the WebDAV toggle — and offers the creator's OWN tiers.
    #[test]
    fn paywall_select_paints_only_on_an_expanded_website_row_with_the_owners_tiers() {
        let mut state = empty_state();
        state.folders.own_tiers = vec!["gold".into(), "silver".into()];
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![website_folder("site")]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-paywall-tier-select").expect("website row paints the select");
        assert!(sel.enabled);
        assert_eq!(sel.path, vec![("folder-row".to_string(), 0)]);
        // Still public ⇒ the "Not paywalled" placeholder is offered first.
        assert_eq!(
            options(sel),
            vec![String::new(), "gold".to_string(), "silver".to_string()]
        );

        // A row whose website toggle is off never paints it.
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        assert!(el(&folders_elements(&state, &[]), "folder-paywall-tier-select").is_none());
    }

    /// No tiers ⇒ disabled with the "create a tier first" hint: there is nothing
    /// to paywall to, so offering the control would be a dead end.
    #[test]
    fn paywall_select_is_disabled_without_any_own_tier() {
        let mut state = empty_state();
        assert!(state.folders.own_tiers.is_empty());
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![website_folder("site")]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-paywall-tier-select").expect("still painted");
        assert!(!sel.enabled);
        assert_eq!(sel.label.as_deref(), Some(t::PAYWALL_TIER_NEEDS_TIER));
    }

    /// v1 is SET-ONLY: once paywalled, the "Not paywalled" placeholder is gone, so
    /// the select cannot present a clear affordance no client implements
    /// (`ui/folders.md:60`).
    #[test]
    fn an_already_paywalled_set_offers_no_public_placeholder() {
        let mut state = empty_state();
        state.folders.own_tiers = vec!["gold".into()];
        state.folders.expanded = Some(0);
        let mut fs = website_folder("site");
        fs.web_paywall_tier = Some("gold".into());
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        let sel = el(&els, "folder-paywall-tier-select").unwrap();
        assert_eq!(sel.text, "gold");
        assert_eq!(options(sel), vec!["gold".to_string()]);
    }

    /// The page-level default is UNINDEXED and distinct from the per-set
    /// `folder-conflict-policy-select`: exactly one of it exists no matter how
    /// many owner rows render. (The e2e reads it with no index at all, so a
    /// second occurrence would make `select` ambiguous.)
    #[test]
    fn sync_default_conflict_policy_select_is_page_level_not_per_row() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("a"), folder("b")]));
        let els = folders_elements(&state, &[]);
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "sync-default-conflict-policy-select")
                .count(),
            1
        );
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "folder-conflict-policy-select")
                .count(),
            2
        );
    }

    /// A machine with no lingering concept (`linger_enabled: None`) gets no
    /// offer at all — the row must not degrade into an off-looking toggle the
    /// user cannot act on. The empty-list test above pins the same absence.
    #[test]
    fn no_linger_reading_paints_no_linger_toggle() {
        let state = empty_state();
        assert!(state.folders.linger_enabled.is_none());
        assert!(el(&folders_elements(&state, &[]), "sync-agent-linger-toggle").is_none());
    }

    /// Off: the toggle renders above the set list, carries `state=off`, and the
    /// help line is the call to action.
    #[test]
    fn linger_off_paints_the_offer_above_the_list() {
        let mut state = empty_state();
        state.folders.linger_enabled = Some(false);
        let els = folders_elements(&state, &[]);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "page-heading",
                "sync-agent-linger-toggle",
                "",
                // the "Sync defaults" group title + its one page-level control,
                // which also sit above the set list
                "",
                "sync-default-conflict-policy-select",
                "folder-add-button",
                // The unconditional "Folders you follow" section (title +
                // button + hint) — see the empty-list test above.
                "",
                "folder-follow-button",
                "",
                "settings-nav-back"
            ],
            "the offer sits above the list, its help line untagged chrome"
        );
        let toggle = el(&els, "sync-agent-linger-toggle").unwrap();
        assert_eq!(toggle.attrs, vec![("state".to_string(), "off".to_string())]);
        assert_eq!(
            els[2].text,
            fs_strings::KEEP_SYNCING_HELP_OFF,
            "while off, the help line must say what turning it on buys"
        );
    }

    /// On: the row STAYS (the toggle is reversible — user-approved 2026-07-23),
    /// flips to `state=on`, and its help line switches to the reassurance.
    #[test]
    fn linger_on_keeps_the_toggle_so_it_can_be_turned_back_off() {
        let mut state = empty_state();
        state.folders.linger_enabled = Some(true);
        let els = folders_elements(&state, &[]);
        let toggle = el(&els, "sync-agent-linger-toggle").unwrap();
        assert_eq!(toggle.attrs, vec![("state".to_string(), "on".to_string())]);
        assert_eq!(els[2].text, fs_strings::KEEP_SYNCING_HELP_ON);
    }

    /// Phase 5 retired the per-row scan-frequency select (`file-sync.md`
    /// § Config, the phase-5 block).
    #[test]
    fn an_owner_row_paints_the_conflict_policy_select_and_no_frequency_select() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-row").is_some());
        assert!(
            el(&els, "folder-frequency-select").is_none(),
            "the scan-frequency select retired with phase 5"
        );
        let policy = el(&els, "folder-conflict-policy-select").unwrap();
        assert_eq!(policy.text, "auto");
        assert_eq!(policy.path, vec![("folder-row".to_string(), 0)]);
    }

    /// The conflict policy is the owner's: a `member` row paints none
    /// (`ui/folders.md` § Conflicts — every OWNER row).
    #[test]
    fn a_member_row_paints_no_conflict_policy_select() {
        let mut state = empty_state();
        let mut shared = folder("shared-with-me");
        shared.role = Some("member".into());
        state.devices.snapshot = Some(snapshot_with_folders(vec![shared]));
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-conflict-policy-select").is_none());
    }

    #[test]
    fn collapsed_row_paints_no_path_editors_or_delete() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-include-paths").is_none());
        assert!(el(&els, "folder-delete-button").is_none());
    }

    #[test]
    fn expanded_row_paints_path_editors_and_delete_but_not_confirm_until_armed() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-include-paths").is_some());
        assert!(el(&els, "folder-exclude-paths").is_some());
        assert!(el(&els, "folder-save-paths").is_some());
        assert!(el(&els, "folder-delete-button").is_some());
        assert!(el(&els, "folder-delete-confirm").is_none());

        state.folders.delete_pending = true;
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-delete-confirm").is_some());
    }

    fn binding(path: &str, folder: &str) -> RenderedLocationBinding {
        RenderedLocationBinding {
            path: path.to_string(),
            folder: folder.to_string(),
            access_revoked: false,
            deletes_held: 0,
            deletes_skipped_unreadable: 0,
            mode: None,
        }
    }

    /// A binding on a host whose agent has a placeholder surface — the only
    /// shape that carries a mode to paint.
    fn moded_binding(path: &str, folder: &str, mode: &str) -> RenderedLocationBinding {
        RenderedLocationBinding {
            mode: Some(RenderedModeToggle {
                on_demand: mode == "on-demand",
                enabled: true,
                notice: None,
            }),
            ..binding(path, folder)
        }
    }

    fn held_binding(path: &str, folder: &str, deletes_held: u64) -> RenderedLocationBinding {
        RenderedLocationBinding {
            deletes_held,
            ..binding(path, folder)
        }
    }

    fn revoked_binding(path: &str, folder: &str) -> RenderedLocationBinding {
        RenderedLocationBinding {
            access_revoked: true,
            ..binding(path, folder)
        }
    }

    fn ids_of<'a>(els: &'a [Element], id: &str) -> Vec<&'a Element> {
        els.iter().filter(|e| e.id == id).collect()
    }

    #[test]
    fn collapsed_row_paints_no_sync_folder_section() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        // A binding exists for the set, but the row is collapsed: no section.
        let els = folders_elements(&state, &[binding("/home/u/Pictures", "photos")]);
        assert!(el(&els, "folder-location-list").is_none());
        assert!(el(&els, "folder-location-add-button").is_none());
        assert!(el(&els, "folder-location-row").is_none());
    }

    #[test]
    fn expanded_row_paints_the_sync_folder_list_and_add_form_even_when_empty() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        state.folders.location_path_input = "/home/u/Docs".to_string();
        // No folders bound yet — the list anchor + add form still render.
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-location-list").is_some());
        assert!(el(&els, "folder-location-row").is_none());
        let input = el(&els, "folder-location-path-input").unwrap();
        assert_eq!(input.text, "/home/u/Docs", "the input paints its buffer");
        assert!(el(&els, "folder-location-add-button").is_some());
        // The OS-picker browse button is a declared tui absence (tui.md § 4).
        assert!(el(&els, "folder-location-browse-button").is_none());
        // Contextual set — no free-text set name field.
        assert!(el(&els, "folder-location-fileset-input").is_none());
        assert!(el(&els, "folder-location-fileset").is_none());
    }

    #[test]
    fn sync_folder_rows_render_only_this_sets_bindings_scoped_within_the_row() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![
            folder("photos"),
            folder("docs"),
        ]));
        // Expand row 0 (photos). Two folders bound to photos, one to docs.
        state.folders.expanded = Some(0);
        let folders = [
            binding("/home/u/Pictures", "photos"),
            binding("/home/u/Camera", "photos"),
            binding("/home/u/Documents", "docs"),
        ];
        let els = folders_elements(&state, &folders);
        // Only the two photos rows render (the docs binding is filtered out).
        let rows = ids_of(&els, "folder-location-row");
        assert_eq!(rows.len(), 2, "only this set's folders render");
        // Each row hangs off the EXPANDED folder row, so the shared suite's
        // `count("folder-location-row", scope="folder-row[0]")` resolves (the GUI
        // apps get this from real widget nesting; tui declares it).
        assert_eq!(rows[0].path, vec![("folder-row".to_string(), 0)]);
        assert_eq!(rows[1].path, vec![("folder-row".to_string(), 0)]);
        let paths = ids_of(&els, "folder-location-path");
        assert_eq!(paths.len(), 2);
        // The path is scoped within its row, under the folder row:
        // folder-row[0] / folder-location-row[k].
        assert_eq!(paths[0].text, "/home/u/Pictures");
        assert_eq!(
            paths[0].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-location-row".to_string(), 0)
            ]
        );
        assert_eq!(
            paths[1].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-location-row".to_string(), 1)
            ]
        );
        // The remove button is scoped in the row and removes BY THIS SET's name
        // (the shared model's only remove key), never a row index.
        let removes = ids_of(&els, "folder-location-remove-button");
        assert_eq!(removes.len(), 2);
        assert_eq!(
            removes[0].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-location-row".to_string(), 0)
            ]
        );
        match &removes[0].role {
            Role::Button(Gesture::Settings(Action::RemoveLocation(name))) => {
                assert_eq!(name, "photos")
            }
            other => panic!("expected RemoveLocation(\"photos\"), got {other:?}"),
        }
    }

    /// `folder-location-mode-toggle` on a host with a placeholder surface
    /// (`on-demand-files.md` § On-Demand Files → *The choice is the user's*):
    /// one switch per bound row, scoped within it, checked while on-demand, the
    /// mode in its `state` attr (the uniform `always|on-demand` read), and its
    /// gesture carrying the OTHER mode for that row's path — the flip is a
    /// deterministic "set to X", never a blind toggle that could race a flip
    /// made from another surface.
    #[test]
    fn a_moded_row_paints_the_mode_switch_scoped_within_it() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let folders = [
            moded_binding("C:\\Users\\u\\Pictures", "photos", "on-demand"),
            moded_binding("C:\\Users\\u\\Camera", "photos", "always"),
        ];
        let els = folders_elements(&state, &folders);
        let toggles = ids_of(&els, "folder-location-mode-toggle");
        assert_eq!(toggles.len(), 2, "one switch per bound row");

        for (k, (toggle, (path, mode, want))) in toggles
            .iter()
            .zip([
                ("C:\\Users\\u\\Pictures", "on-demand", "always"),
                ("C:\\Users\\u\\Camera", "always", "on-demand"),
            ])
            .enumerate()
        {
            assert_eq!(
                toggle.path,
                vec![
                    ("folder-row".to_string(), 0),
                    ("folder-location-row".to_string(), k)
                ]
            );
            assert_eq!(toggle.attrs, vec![("state".to_string(), mode.to_string())]);
            match &toggle.role {
                Role::Checkbox {
                    gesture: Gesture::Settings(Action::SetLocationMode { path: p, mode: m }),
                    checked,
                } => {
                    assert_eq!(p, path);
                    assert_eq!(m, want, "the gesture sets the OTHER mode");
                    assert_eq!(*checked, mode == "on-demand");
                }
                other => panic!("expected a SetLocationMode checkbox, got {other:?}"),
            }
        }
    }

    /// Where the agent cannot serve on-demand (a linux host without `fuse3`)
    /// an always-resident row keeps its switch — disabled, so the gesture
    /// cannot fire — with the host's reason painted under it as untagged text.
    #[test]
    fn an_unservable_row_paints_the_switch_disabled_with_its_reason() {
        // `OnDemandNotice::NeedsFuse3.i18n_key()`, spelled out: this module
        // compiles where the agent types do not.
        let notice = "devices.sync_locations.on_demand_needs_fuse3";
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let folders = [RenderedLocationBinding {
            mode: Some(RenderedModeToggle {
                on_demand: false,
                enabled: false,
                notice: Some(notice),
            }),
            ..binding("/home/u/Pictures", "photos")
        }];
        let els = folders_elements(&state, &folders);
        let toggle = el(&els, "folder-location-mode-toggle").expect("the switch still renders");
        assert!(
            !toggle.enabled,
            "a flip that cannot be served is not offered"
        );
        assert_eq!(
            toggle.attrs,
            vec![("state".to_string(), "always".to_string())]
        );
        let line = fauna_i18n::strings::lookup(notice).expect("the notice has a line");
        assert!(
            els.iter().any(|e| e.id.is_empty() && e.text == line),
            "the reason is painted, untagged"
        );
    }

    /// No switch for this host (`mode: None` — macOS, where on-demand is not
    /// the agent's) → no switch at all, never an inert one.
    #[test]
    fn a_row_without_a_mode_paints_no_mode_switch() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[binding("/home/u/Pictures", "photos")]);
        assert!(el(&els, "folder-location-row").is_some());
        assert!(el(&els, "folder-location-mode-toggle").is_none());
    }

    /// The mass-delete floor's confirm affordance renders on the held row and
    /// **only** there (`delete-propagation.md` § A wholesale-vanished folder is
    /// infrastructure failure). The hold is per set, so a second folder bound to
    /// the same set carries it too — and an unheld set's rows must stay silent,
    /// which is the failure mode that matters: a stray "apply deletions" button
    /// on a healthy folder is an offer to destroy files that are right there.
    #[test]
    fn a_held_row_paints_the_count_and_the_apply_verb_scoped_within_it() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let folders = [
            held_binding("/mnt/ext/Pictures", "photos", 12),
            binding("/home/u/Camera", "photos"),
        ];
        let els = folders_elements(&state, &folders);

        let held = ids_of(&els, "folder-location-deletes-held");
        assert_eq!(held.len(), 1, "only the held row paints the line");
        assert_eq!(
            held[0].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-location-row".to_string(), 0)
            ],
            "the line is scoped within its own binding row"
        );
        assert!(
            held[0].text.contains("12"),
            "the line must name the count; got {:?}",
            held[0].text
        );

        let apply = ids_of(&els, "folder-location-apply-deletes-button");
        assert_eq!(
            apply.len(),
            1,
            "the verb rides the same condition as the line"
        );
        assert_eq!(
            apply[0].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-location-row".to_string(), 0)
            ]
        );
        // The gesture carries the SET, never the rendered count — the agent
        // re-derives what is missing at click time.
        match &apply[0].role {
            Role::Button(Gesture::Settings(Action::ApplyHeldDeletes(name))) => {
                assert_eq!(name, "photos")
            }
            other => panic!("expected ApplyHeldDeletes(\"photos\"), got {other:?}"),
        }
    }

    /// A row whose set reports an unreadable subtree paints
    /// `folder-location-unreadable` with the count, scoped within its own
    /// binding row — and **no** action, not even the hold's apply verb: there is
    /// nothing to confirm (`delete-propagation.md` § Unreadable is not absent).
    /// A healthy sibling row stays silent, and a zero paints nothing.
    #[test]
    fn an_unreadable_row_paints_the_count_and_no_action() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let folders = [
            RenderedLocationBinding {
                deletes_skipped_unreadable: 8,
                ..binding("/mnt/nas/Pictures", "photos")
            },
            binding("/home/u/Camera", "photos"),
        ];
        let els = folders_elements(&state, &folders);

        let line = ids_of(&els, "folder-location-unreadable");
        assert_eq!(line.len(), 1, "only the unreadable row paints the line");
        assert_eq!(
            line[0].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-location-row".to_string(), 0)
            ],
            "the line is scoped within its own binding row"
        );
        assert_eq!(line[0].text, fs_strings::unreadable("8"));
        assert!(
            ids_of(&els, "folder-location-apply-deletes-button").is_empty()
                && ids_of(&els, "folder-location-deletes-held").is_empty(),
            "an unreadable path is not a hold: no hold line, no apply verb"
        );

        let els = folders_elements(&state, &[binding("/mnt/nas/Pictures", "photos")]);
        assert!(
            ids_of(&els, "folder-location-unreadable").is_empty(),
            "a zero report paints nothing"
        );
    }

    /// `0` is the overwhelmingly common reading and the only thing that ever
    /// retracts a hold, so it must paint **nothing** — not a zeroed line, not a
    /// disabled button. Pins the retraction direction the shared model's
    /// `a_zero_report_clears_a_displayed_hold` pins one layer down.
    #[test]
    fn an_unheld_row_paints_neither_the_line_nor_the_verb() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[binding("/home/u/Pictures", "photos")]);
        assert!(el(&els, "folder-location-deletes-held").is_none());
        assert!(el(&els, "folder-location-apply-deletes-button").is_none());
    }

    /// The expander BODY (path editors, delete affordance, the whole
    /// `folder-location-*` section) is scoped under its own `folder-row[i]` — the
    /// containment the shared suite addresses by `scope="folder-row[i]"`, and
    /// which a top-level registration silently breaks (a scoped query never
    /// matches an empty path, so the count reads 0 while the element paints).
    /// Pins the SECOND row too, so the scope index tracks the expanded row rather
    /// than being hard-coded to 0.
    #[test]
    fn the_expanded_body_is_scoped_under_its_own_folder_row() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![
            folder("photos"),
            folder("docs"),
        ]));
        state.folders.expanded = Some(1);
        state.folders.delete_pending = true;
        let els = folders_elements(&state, &[binding("/home/u/Documents", "docs")]);
        for id in [
            "folder-include-paths",
            "folder-exclude-paths",
            "folder-save-paths",
            "folder-delete-button",
            "folder-delete-confirm",
            "folder-location-list",
            "folder-location-row",
            "folder-location-path-input",
            "folder-location-add-button",
        ] {
            let e =
                el(&els, id).unwrap_or_else(|| panic!("{id} should render on the expanded row"));
            assert_eq!(
                e.path,
                vec![("folder-row".to_string(), 1)],
                "{id} must be scoped under the expanded folder-row"
            );
        }
    }

    #[test]
    fn sync_folder_add_button_dispatches_add_sync_folder() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[]);
        let add = el(&els, "folder-location-add-button").unwrap();
        assert!(matches!(
            add.role,
            Role::Button(Gesture::Settings(Action::AddLocation))
        ));
    }

    // ── Exclusive editing (ui/folders.md § Exclusive editing) ──────────────

    fn lease_device(id: &str, label: &str) -> fauna_devices_machine::DeviceSummary {
        fauna_devices_machine::DeviceSummary {
            device_id: id.into(),
            label: label.into(),
            capabilities: String::new(),
            registered_at: 0,
            last_seen_at: 0,
            online: true,
            guardian_marked: false,
            folders: vec![],
            principal: None,
            p2p_participation: None,
            p2p_off_requested: false,
            p2p_participation_paint: None,
        }
    }

    fn leased(governed: bool, holder: Option<&str>) -> FolderSummary {
        FolderSummary {
            exclusive_editing: governed,
            lease: holder.map(|id| fauna_devices_machine::FolderLeaseSummary {
                device_id: id.into(),
                // Far enough ahead that no test run reaches it.
                expires_at: i64::MAX,
            }),
            ..folder("db")
        }
    }

    fn lease_state(fs: FolderSummary) -> SettingsState {
        let mut state = empty_state();
        state.devices.local_device_id = Some("aa11".into());
        state.devices.snapshot = Some(fauna_devices_machine::DevicesSnapshot {
            devices: vec![
                lease_device("aa11", "My laptop"),
                lease_device("bb22", "Studio desktop"),
            ],
            folders: vec![fs],
            ..Default::default()
        });
        state
    }

    /// The toggle is the owner's, on the expanded body, and paints the nest's
    /// value — off by default.
    #[test]
    fn exclusive_editing_toggle_paints_the_nests_value_on_the_owner_body() {
        let mut state = lease_state(leased(false, None));
        assert!(
            el(
                &folders_elements(&state, &[]),
                "folder-exclusive-editing-toggle"
            )
            .is_none(),
            "a collapsed row has no body"
        );
        state.folders.expanded = Some(0);
        for on in [false, true] {
            if let Some(snap) = state.devices.snapshot.as_mut() {
                snap.folders[0].exclusive_editing = on;
            }
            let els = folders_elements(&state, &[]);
            let toggle = el(&els, "folder-exclusive-editing-toggle").expect("owner body");
            assert_eq!(toggle.path, vec![("folder-row".to_string(), 0)]);
            match &toggle.role {
                Role::Checkbox { checked, gesture } => {
                    assert_eq!(*checked, on);
                    assert!(matches!(
                        gesture,
                        Gesture::Settings(Action::ToggleFolderExclusiveEditing(0))
                    ));
                }
                other => panic!("not a checkbox: {other:?}"),
            }
        }
    }

    /// The status line is absent while the folder is un-governed, and on a
    /// governed one it reads free / this device / the holder's LABEL — on the
    /// row header, so it paints without expanding.
    #[test]
    fn lease_status_reads_the_projection_and_names_the_holder_by_label() {
        let status = |fs| {
            let els = folders_elements(&lease_state(fs), &[]);
            el(&els, "folder-lease-status").map(|e| (e.text.clone(), e.path.clone()))
        };
        assert_eq!(status(leased(false, Some("bb22"))), None);
        let row0 = vec![("folder-row".to_string(), 0)];
        assert_eq!(
            status(leased(true, None)),
            Some((t::FOLDER_LEASE_FREE.to_string(), row0.clone()))
        );
        assert_eq!(
            status(leased(true, Some("aa11"))),
            Some((t::FOLDER_LEASE_HELD_HERE.to_string(), row0.clone()))
        );
        let (text, path) = status(leased(true, Some("bb22"))).expect("held elsewhere");
        assert!(text.starts_with("Studio desktop is editing"), "{text}");
        assert!(!text.contains("bb22"), "never the device id: {text}");
        assert_eq!(path, row0);
    }

    /// A reader member sees the lock too — it is the seat that most needs to
    /// know it cannot upload — but never the owner's toggle.
    #[test]
    fn a_member_row_shows_the_lease_status_but_not_the_toggle() {
        let mut fs = leased(true, Some("cc33"));
        fs.role = Some("member".into());
        let mut state = lease_state(fs);
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-exclusive-editing-toggle").is_none());
        assert_eq!(
            el(&els, "folder-lease-status").map(|e| e.text.as_str()),
            Some(t::FOLDER_LEASE_HELD_ELSEWHERE)
        );
    }

    /// The device-local folder-binding section is OWNER + WRITER-MEMBER only
    /// (`ui/folders.md:152` — "everything else... stays owner-only", and "A
    /// writer additionally binds local folders"). A member row with no explicit
    /// `writer` access is a **reader** by construction (fail-safe default), and
    /// must render none of the owner-only body — mirrors linux's
    /// `build_member_folder_row` (plain read-only row, no expander body).
    #[test]
    fn a_reader_member_row_expanded_shows_no_owner_controls_and_no_location_binding() {
        let mut fs = folder("theirs");
        fs.role = Some("member".into());
        fs.owner_display = "alice".into();
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[binding("/home/u/Pictures", "theirs")]);
        for id in [
            "folder-conflict-policy-select",
            "folder-include-paths",
            "folder-exclude-paths",
            "folder-save-paths",
            "folder-delete-button",
            "folder-webdav-toggle",
            "folder-share-button",
            "folder-device-activity-item",
            "folder-location-list",
            "folder-location-row",
            "folder-location-add-button",
        ] {
            assert!(
                el(&els, id).is_none(),
                "{id} must not render on a reader-access member row"
            );
        }
        // The recipient header surfaces still render — this is a body gap, not a
        // rollback of the already-tested header behavior.
        assert!(el(&els, "folder-shared-badge").is_some());
    }

    /// A **writer** member row gets exactly one management affordance — the
    /// folder-binding widget — and none of the owner-only controls (schedule,
    /// paths, delete, webdav, share). `ui/folders.md:152`; mirrors linux's
    /// `build_writer_member_folder_row`.
    #[test]
    fn a_writer_member_row_expanded_shows_only_the_location_binding_section() {
        let mut fs = folder("theirs");
        fs.role = Some("member".into());
        fs.access = Some("writer".into());
        fs.owner_display = "alice".into();
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[binding("/home/u/Pictures", "theirs")]);
        for id in [
            "folder-conflict-policy-select",
            "folder-include-paths",
            "folder-exclude-paths",
            "folder-save-paths",
            "folder-delete-button",
            "folder-webdav-toggle",
            "folder-share-button",
            "folder-device-activity-item",
        ] {
            assert!(
                el(&els, id).is_none(),
                "{id} must not render on a writer-access member row"
            );
        }
        assert!(el(&els, "folder-location-list").is_some());
        assert!(el(&els, "folder-location-row").is_some());
        assert!(el(&els, "folder-location-add-button").is_some());
        assert!(el(&els, "folder-access-revoked-warning").is_none());
    }

    /// D4 — a writer whose grant was revoked mid-life sees the park warning
    /// ABOVE the (still-visible, still-removable) binding rows, never a silent
    /// stop. `ui/folders.md:152`, `file-sync.md` § Multi-writer shared sets.
    #[test]
    fn a_writer_member_row_with_a_revoked_binding_shows_the_park_warning() {
        let mut fs = folder("theirs");
        fs.role = Some("member".into());
        fs.access = Some("writer".into());
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[revoked_binding("/home/u/Pictures", "theirs")]);
        assert!(el(&els, "folder-access-revoked-warning").is_some());
        // The park is not a deletion — the bound folder still renders, removable.
        assert!(el(&els, "folder-location-row").is_some());
    }

    /// The same park AFTER the folder list has caught up with the demotion: the
    /// row's access now reads `reader`. The warning and the parked binding must
    /// still render — this is the state the user actually opens the page on,
    /// and keying the section on the current access alone hid both of them.
    #[test]
    fn a_demoted_members_row_still_shows_the_park_warning_and_the_parked_binding() {
        let mut fs = folder("theirs");
        fs.role = Some("member".into());
        fs.access = Some("reader".into());
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[revoked_binding("/home/u/Pictures", "theirs")]);
        assert!(el(&els, "folder-access-revoked-warning").is_some());
        assert!(el(&els, "folder-location-row").is_some());
        // Still a member row: none of the owner's controls appear with it.
        assert!(el(&els, "folder-share-button").is_none());
        assert!(el(&els, "folder-delete-button").is_none());
    }

    /// Regression guard for the reader/writer restructure above: an owner
    /// (non-member) row is unaffected.
    #[test]
    fn an_owner_row_still_gets_the_conflict_policy_select() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("mine")]));
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-conflict-policy-select").is_some());
    }

    /// A wizard snapshot fixture. `name` empty means the name gate is closed.
    /// Every field the wizard steps read, so a test varies only what it asserts.
    fn wizard_fixture(
        step: FolderWizardStep,
        name: &str,
        devices: Vec<WizardDevice>,
    ) -> FolderWizardSnapshot {
        // Every flag point is a valid place, so no seat closes this step.
        let continue_enabled = true;
        FolderWizardSnapshot {
            step,
            name: NameSnapshot {
                name: name.to_string(),
                continue_enabled: !name.is_empty(),
            },
            device_places: DevicePlacesSnapshot {
                devices,
                continue_enabled,
            },
            review: ReviewSnapshot {
                name: name.to_string(),
                retention: None,
                enrolled: Vec::new(),
                create_enabled: !name.is_empty(),
                phase: SubmitPhase::Idle,
                created: false,
                failed_members: Vec::new(),
                error: None,
            },
        }
    }

    /// A wizard device seat at flag point `f`.
    fn seat(label: &str, selected: bool, f: fauna_protocol::folders::PlaceFlags) -> WizardDevice {
        WizardDevice {
            device_id: "aa".repeat(32),
            label: label.into(),
            selected,
            originates: f.originates,
            accepts: f.accepts,
            applies_deletes: f.applies_deletes,
        }
    }

    #[test]
    fn wizard_open_paints_only_wizard_elements() {
        let mut snapshot = snapshot_with_folders(vec![folder("photos")]);
        snapshot.wizard = Some(wizard_fixture(FolderWizardStep::Name, "", Vec::new()));
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot);
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "wizard-name-input").is_some());
        assert!(el(&els, "folder-add-button").is_none());
        assert!(el(&els, "folder-row").is_none());
        // A folder has no type: the mode step is gone with the mode itself
        // (phase 2 slice e). These IDs are retired from ui.yaml — painting one
        // would be an ID the spec does not declare.
        for retired in [
            "wizard-mode-sync",
            "wizard-mode-backup",
            "wizard-mode-web",
            "wizard-mode-backup-warning",
        ] {
            assert!(el(&els, retired).is_none(), "{retired} is retired");
        }
        // Back is hidden on the first step; Next is present, disabled (empty name).
        assert!(el(&els, "wizard-back-button").is_none());
        let next = el(&els, "wizard-next-button").unwrap();
        assert!(!next.enabled);
    }

    /// Step 1's legibility contract (the 2026-08-03 comprehensibility audit's
    /// second seed finding, a live-user "completely unclear" report): the
    /// disabled Next explains itself while the name gate is closed, and the
    /// name input prompts with a human label. The mode radios this test used to
    /// cover went with the mode itself in phase 2 slice e.
    #[test]
    fn wizard_name_step_is_legible() {
        let mut snapshot = snapshot_with_folders(vec![]);
        snapshot.wizard = Some(wizard_fixture(FolderWizardStep::Name, "", Vec::new()));
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot);
        let els = folders_elements(&state, &[]);
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();

        // The disabled Next explains itself while the name gate is closed…
        assert!(
            texts.contains(&fauna_i18n::strings::devices::wizard::NAME_REQUIRED),
            "empty name → the why-is-Next-disabled line must render"
        );
        // …and the name input prompts with a human label, not its element id.
        let name = el(&els, "wizard-name-input").unwrap();
        assert_eq!(
            name.label.as_deref(),
            Some(fauna_i18n::strings::devices::wizard::NAME_LABEL)
        );

        // Name gate open → the hint disappears.
        {
            let wizard = state
                .devices
                .snapshot
                .as_mut()
                .unwrap()
                .wizard
                .as_mut()
                .unwrap();
            wizard.name.name = "photos".into();
            wizard.name.continue_enabled = true;
        }
        let els = folders_elements(&state, &[]);
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();
        assert!(
            !texts.contains(&fauna_i18n::strings::devices::wizard::NAME_REQUIRED),
            "valid name → no stale required-hint"
        );

        // The wizard is three steps since phase 5 — there is no cadence step,
        // and the review paints no cadence line either.
        {
            let wizard = state
                .devices
                .snapshot
                .as_mut()
                .unwrap()
                .wizard
                .as_mut()
                .unwrap();
            wizard.step = FolderWizardStep::Review;
        }
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "wizard-frequency-option").is_none());
        assert!(el(&els, "wizard-create-button").is_some());
        assert!(
            !els.iter().any(|e| e.text.contains("Frequency")),
            "the review must not paint a cadence line"
        );
    }

    /// The device step's legibility contract (a live-user field report,
    /// 2026-08-05: "a completely incomprehensible list of things" — bare
    /// one-word role labels with no explanation of what each does). Phase 2
    /// slice e's answer is three checkboxes that each SAY what they do, so the
    /// contract is now: every flag box paints, each carries its own explainer,
    /// and the step still announces itself as optional.
    #[test]
    fn wizard_device_places_step_is_legible() {
        let mut snapshot = snapshot_with_folders(vec![]);
        snapshot.wizard = Some(wizard_fixture(
            FolderWizardStep::Devices,
            "photos",
            vec![seat(
                "this device",
                false,
                fauna_protocol::folders::PlaceFlags::archive_place(),
            )],
        ));
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot);
        let els = folders_elements(&state, &[]);
        let texts: Vec<&str> = els.iter().map(|e| e.text.as_str()).collect();

        // The step header states the step is optional — the exact gap the
        // live-user report hit: nothing on screen said skipping was fine.
        assert!(
            texts.contains(&fauna_i18n::strings::devices::wizard::SELECT_DEVICES_ROLES),
            "the step header must render; got {texts:?}"
        );
        assert!(
            fauna_i18n::strings::devices::wizard::SELECT_DEVICES_ROLES
                .to_ascii_lowercase()
                .contains("optional"),
            "the header itself must say the step is optional, not just exist"
        );

        // All three flag boxes paint, each in the state the seat's flags say.
        // `backup` = originates + accepts, never deletes (the 2026-08-19
        // ruling: an archive seat contributes its own files; only its delete
        // posture makes it the archive).
        for (id, on) in [
            ("wizard-device-originates", true),
            ("wizard-device-accepts", true),
            ("wizard-device-applies-deletes", false),
        ] {
            let e = el(&els, id).unwrap_or_else(|| panic!("{id} must paint"));
            match &e.role {
                Role::Checkbox { checked, .. } => assert_eq!(*checked, on, "{id} checked state"),
                other => panic!("{id} should be a checkbox, got {other:?}"),
            }
        }
        // The retired picker is gone — its ID is no longer in ui.yaml.
        assert!(el(&els, "wizard-device-role").is_none());

        // EVERY box carries its own explainer — unlike the role picker, where
        // only the selected option's desc rendered. The boxes are independent,
        // so a user deciding about one needs that one explained.
        for desc in [
            fauna_i18n::strings::devices::wizard::PLACE_ORIGINATES_DESC,
            fauna_i18n::strings::devices::wizard::PLACE_ACCEPTS_DESC,
            fauna_i18n::strings::devices::wizard::PLACE_APPLIES_DELETES_DESC,
        ] {
            assert!(texts.contains(&desc), "missing explainer {desc:?}");
        }

        // Zero devices checked is a valid, advanceable state (folders.md
        // § Create wizard step 2: "enrolling zero devices is allowed").
        let next = el(&els, "wizard-next-button").unwrap();
        assert!(
            next.enabled,
            "Next must stay enabled with nothing enrolled — this step is optional"
        );
    }

    /// The nest place's editor renders on EVERY folder — not just a backup-type
    /// one, which is the whole point of moving retention off the wizard
    /// (`backup-restore.md` § 8b; plan doc § Cadence split).
    #[test]
    fn the_nest_place_editor_renders_on_an_ordinary_folder() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("photos")]));
        state.folders.expanded = Some(0);
        let els = folders_elements(&state, &[]);
        for id in [
            "folder-nest-snapshots-select",
            "folder-nest-quiet-input",
            "folder-nest-retention-snapshots",
            "folder-nest-retention-days",
            "folder-nest-save-button",
        ] {
            assert!(el(&els, id).is_some(), "{id} must render on a sync folder");
        }
    }

    /// Every knob is three-state and **unset is the resting value**: an
    /// untouched folder shows "use the default" and BLANK boxes, never `0`.
    /// A rendered `0` would read as a real bound — and for retention it would
    /// read as "keep zero snapshots", the one reading that destroys history.
    #[test]
    fn an_untouched_nest_place_renders_unset_not_zero() {
        let mut ui = FoldersUiState::default();
        let fs = folder("photos");
        assert_eq!(fs.nest_snapshots, None, "fixture must rest unset");
        prefill_paths(&mut ui, Some(&fs));

        assert_eq!(ui.nest_snapshots_input, NEST_SNAPSHOTS_DEFAULT);
        assert_eq!(ui.nest_quiet_input, "");
        assert_eq!(ui.nest_retention_snapshots_input, "");
        assert_eq!(ui.nest_retention_days_input, "");
    }

    /// Both retention boxes blank CLEARS the policy (keep everything). One box
    /// blank rides as the nest's own "this bound is unset" spelling — a zero —
    /// which `backup/retention.rs::parse_folder_retention` reads as unset and
    /// never as "keep zero".
    /// Emptying both boxes must actually CLEAR the policy on the nest — and the
    /// only way to say that is the canonical binds-nothing value, because
    /// `FolderUpdateRequest::retention_policy`'s `None` means "leave unchanged".
    /// This is the exact regression the tier_3 clear leg caught: returning
    /// `None` here let a user set retention and never take it back.
    #[test]
    fn emptying_both_retention_boxes_sends_a_policy_that_binds_nothing() {
        let cleared = retention_from_inputs("", "").expect(
            "both blank must send the canonical binds-nothing policy, NOT None \
             (None means `leave unchanged` on the wire)",
        );
        let parsed: RetentionPolicy = serde_json::from_str(&cleared).unwrap();
        assert_eq!((parsed.max_snapshots, parsed.max_age_days), (0, 0));

        // And it round-trips back to two blank boxes, not two zeroes.
        let mut ui = FoldersUiState::default();
        let mut fs = folder("photos");
        fs.retention_policy = Some(cleared);
        prefill_paths(&mut ui, Some(&fs));
        assert_eq!(ui.nest_retention_snapshots_input, "");
        assert_eq!(ui.nest_retention_days_input, "");
    }

    #[test]
    fn a_blank_retention_bound_rides_as_zero_never_as_keep_zero() {
        let only_count = retention_from_inputs("7", "").expect("a set bound must ride");
        let parsed: RetentionPolicy = serde_json::from_str(&only_count).unwrap();
        assert_eq!(parsed.max_snapshots, 7);
        assert_eq!(
            parsed.max_age_days, 0,
            "a blank bound must ride as 0 = unset, which the nest ignores"
        );

        // And the round trip does not turn an unset bound into a rendered 0.
        let mut ui = FoldersUiState::default();
        let mut fs = folder("photos");
        fs.retention_policy = Some(only_count);
        prefill_paths(&mut ui, Some(&fs));
        assert_eq!(ui.nest_retention_snapshots_input, "7");
        assert_eq!(ui.nest_retention_days_input, "");
    }

    /// Every flag point is an ordinary place: the step stays open and no
    /// refusal line is painted.
    #[test]
    fn no_flag_point_closes_the_step() {
        let unnameable = seat(
            "this device",
            true,
            fauna_protocol::folders::PlaceFlags::new(false, true, false),
        );

        let mut snapshot = snapshot_with_folders(vec![]);
        snapshot.wizard = Some(wizard_fixture(
            FolderWizardStep::Devices,
            "photos",
            vec![unnameable],
        ));
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot);
        let els = folders_elements(&state, &[]);

        let next = el(&els, "wizard-next-button").unwrap();
        assert!(next.enabled, "no flag point closes the step any more");
    }

    // ── Cross-user sharing (`ui/folders.md` § Sharing a folder) ─────────

    fn member(
        actor_id: &str,
        handle: &str,
        role: &str,
    ) -> fauna_protocol::folders::FolderActorMember {
        fauna_protocol::folders::FolderActorMember {
            actor_id: actor_id.to_string(),
            handle: handle.to_string(),
            role: role.to_string(),
            ..Default::default()
        }
    }

    /// A state with one owner set expanded and `members` read for it.
    fn owner_state_with_roster(
        members: Vec<fauna_protocol::folders::FolderActorMember>,
    ) -> SettingsState {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("s")]));
        state.folders.expanded = Some(0);
        state.folders.member_cap_inputs = fauna_client_folders::member_actors(&members)
            .into_iter()
            .map(|m| m.byte_cap.map(|c| c.to_string()).unwrap_or_default())
            .collect();
        state.folders.members = members;
        state.folders.roster_for = Some("s".into());
        state.folders.roster_channel_id = Some("aa".repeat(32));
        state
    }

    #[test]
    fn an_unshared_owner_row_offers_share_but_paints_no_badge_and_no_members() {
        let state = owner_state_with_roster(Vec::new());
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-share-button").is_some());
        assert!(
            el(&els, "folder-shared-badge").is_none(),
            "an owner-only set must show no Shared · N badge"
        );
        assert!(el(&els, "folder-member-item").is_none());
        // The form is only armed by the gesture — not open on expand.
        assert!(el(&els, "recipient-picker-input").is_none());
        assert!(el(&els, "folder-share-confirm").is_none());
    }

    #[test]
    fn the_share_form_reuses_the_recipient_picker_id_and_defaults_to_reader() {
        let mut state = owner_state_with_roster(Vec::new());
        state.folders.share_open = true;
        state.folders.share_access = "reader".into();
        let els = folders_elements(&state, &[]);
        // `ui/folders.md:147` — the picker is REUSED; no new picker ids exist.
        assert!(el(&els, "recipient-picker-input").is_some());
        assert!(el(&els, "folder-share-confirm").is_some());
        let role = el(&els, "folder-share-role-select").unwrap();
        assert_eq!(role.text, "reader");
        assert_eq!(
            options(role),
            vec!["reader".to_string(), "writer".to_string()]
        );
        assert!(
            el(&els, "folder-writer-uncapped-warning").is_none(),
            "a Reader grant carries no uncapped-writer warning"
        );
    }

    #[test]
    fn picking_writer_in_the_share_form_warns_because_a_share_time_grant_is_uncapped() {
        let mut state = owner_state_with_roster(Vec::new());
        state.folders.share_open = true;
        state.folders.share_access = "writer".into();
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-writer-uncapped-warning").is_some());
    }

    /// The badge count and the rendered rows both come from the ONE shared
    /// `member_actors` filter (`ui/folders.md:175`), so an owner row in the
    /// reply can never inflate the count or shift the list.
    #[test]
    fn the_badge_counts_members_only_and_never_the_owner_row() {
        let state = owner_state_with_roster(vec![
            member("aa", "owner", "owner"),
            member("bb", "bob", "member"),
        ]);
        let els = folders_elements(&state, &[]);
        let badge = el(&els, "folder-shared-badge").unwrap();
        assert!(
            badge.text.contains('1'),
            "one member + one owner row must read 'Shared · 1', got {:?}",
            badge.text
        );
        assert_eq!(
            els.iter().filter(|e| e.id == "folder-member-item").count(),
            1,
            "the owner row is not a shared-with member"
        );
        assert_eq!(el(&els, "folder-member-handle").unwrap().text, "bob");
        assert_eq!(el(&els, "folder-member-status").unwrap().text, "Active");
    }

    /// The member children hang under `folder-member-item[j]`, and the caller
    /// re-scopes the body under `folder-row[i]` — together the
    /// `folder-row[0]/folder-member-item[0]` path the shared helper queries.
    #[test]
    fn member_children_are_scoped_under_their_item_inside_the_row() {
        let state = owner_state_with_roster(vec![member("bb", "bob", "member")]);
        let els = folders_elements(&state, &[]);
        let handle = el(&els, "folder-member-handle").unwrap();
        let path: Vec<(String, usize)> = handle.path.to_vec();
        assert_eq!(
            path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-member-item".to_string(), 0)
            ]
        );
    }

    #[test]
    fn an_uncapped_writer_row_warns_and_a_capped_one_does_not() {
        let mut m = member("bb", "bob", "member");
        m.access = Some("writer".into());
        let state = owner_state_with_roster(vec![m.clone()]);
        assert!(
            el(
                &folders_elements(&state, &[]),
                "folder-writer-uncapped-warning"
            )
            .is_some(),
            "an uncapped writer must show the quota warning"
        );

        let mut capped = m;
        capped.byte_cap = Some(4096);
        let state = owner_state_with_roster(vec![capped]);
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-writer-uncapped-warning").is_none());
        assert_eq!(el(&els, "folder-member-cap-input").unwrap().text, "4096");
    }

    // ── The published-folder writer warning (`ui/folders.md` § Sharing) ──
    // Reach, not the website toggle: `public` or paywalled. State-based, so the
    // share form and the member row both paint it and it stacks with the
    // uncapped warning rather than replacing it.

    /// `owner_state_with_roster`, but with the one set published `public`.
    fn public_owner_state(
        members: Vec<fauna_protocol::folders::FolderActorMember>,
    ) -> SettingsState {
        let mut state = owner_state_with_roster(members);
        let mut fs = folder("s");
        fs.audience = "public".into();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        state
    }

    #[test]
    fn picking_writer_in_the_share_form_of_a_public_folder_warns_it_is_published() {
        let mut state = public_owner_state(Vec::new());
        state.folders.share_open = true;
        state.folders.share_access = "writer".into();
        let els = folders_elements(&state, &[]);
        let warning = el(&els, "folder-writer-published-warning")
            .expect("a Writer grant on a public folder must say it reaches beyond the set");
        assert_eq!(warning.text, t::WRITER_PUBLIC_WARNING);
        assert!(
            el(&els, "folder-writer-uncapped-warning").is_some(),
            "the two warnings name different consequences and stack — neither suppresses the other"
        );
    }

    #[test]
    fn a_reader_grant_or_an_unpublished_folder_carries_no_published_warning() {
        // Reader on a public folder: nothing they can do changes what anyone reads.
        let mut state = public_owner_state(Vec::new());
        state.folders.share_open = true;
        state.folders.share_access = "reader".into();
        assert!(
            el(
                &folders_elements(&state, &[]),
                "folder-writer-published-warning"
            )
            .is_none()
        );

        // Writer on a folder that reaches nobody outside its members.
        let mut state = owner_state_with_roster(Vec::new());
        state.folders.share_open = true;
        state.folders.share_access = "writer".into();
        assert!(
            el(
                &folders_elements(&state, &[]),
                "folder-writer-published-warning"
            )
            .is_none(),
            "a private folder is readable by nobody outside its members"
        );
    }

    #[test]
    fn a_paywalled_folder_names_subscribers_not_anyone() {
        let mut state = owner_state_with_roster(Vec::new());
        let mut fs = folder("s");
        fs.web_paywall_tier = Some("gold".into());
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        state.folders.share_open = true;
        state.folders.share_access = "writer".into();
        let els = folders_elements(&state, &[]);
        assert_eq!(
            el(&els, "folder-writer-published-warning")
                .expect("paywalled content is readable beyond the set")
                .text,
            t::WRITER_PAYWALLED_WARNING
        );
    }

    #[test]
    fn a_writer_row_on_a_public_folder_warns_even_when_capped() {
        let mut m = member("bb", "bob", "member");
        m.access = Some("writer".into());
        m.byte_cap = Some(4096);
        let state = public_owner_state(vec![m]);
        let els = folders_elements(&state, &[]);
        let warning = el(&els, "folder-writer-published-warning")
            .expect("a byte cap bounds the quota, not what the member can publish");
        assert_eq!(warning.text, t::WRITER_PUBLIC_WARNING);
        assert_eq!(
            warning.path.to_vec(),
            vec![
                ("folder-row".to_string(), 0),
                ("folder-member-item".to_string(), 0)
            ],
            "it hangs under the member it is about"
        );
        assert!(
            el(&els, "folder-writer-uncapped-warning").is_none(),
            "the cap clears the quota warning and leaves the published one"
        );
    }

    #[test]
    fn an_uncapped_writer_on_a_public_folder_gets_both_warnings() {
        let mut m = member("bb", "bob", "member");
        m.access = Some("writer".into());
        let state = public_owner_state(vec![m]);
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-writer-published-warning").is_some());
        assert!(el(&els, "folder-writer-uncapped-warning").is_some());
    }

    #[test]
    fn a_reader_row_on_a_public_folder_carries_no_published_warning() {
        let state = public_owner_state(vec![member("bb", "bob", "member")]);
        assert!(
            el(
                &folders_elements(&state, &[]),
                "folder-writer-published-warning"
            )
            .is_none()
        );
    }

    /// A roster read for a DIFFERENT set must never paint under this row — the
    /// guard that makes a fast row-to-row toggle safe.
    #[test]
    fn a_roster_read_for_another_set_is_not_painted() {
        let mut state = owner_state_with_roster(vec![member("bb", "bob", "member")]);
        state.folders.roster_for = Some("some-other-set".into());
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-member-item").is_none());
        assert!(el(&els, "folder-shared-badge").is_none());
    }

    /// With no channel id the set cannot be addressed for an evict, so the
    /// button renders INERT rather than failing on click (linux's own arm).
    #[test]
    fn remove_is_disabled_without_a_channel_id() {
        let mut state = owner_state_with_roster(vec![member("bb", "bob", "member")]);
        state.folders.roster_channel_id = None;
        let els = folders_elements(&state, &[]);
        assert!(!el(&els, "folder-member-remove-button").unwrap().enabled);
    }

    /// Sharing is OWNER-ONLY (`ui/folders.md:152`) — a member row gets the
    /// recipient surfaces instead, on the row header so they read without an
    /// expand.
    #[test]
    fn a_member_row_gets_the_recipient_badge_and_leave_but_no_share_affordance() {
        let mut fs = folder("theirs");
        fs.role = Some("member".into());
        fs.owner_display = "alice".into();
        fs.mls_group_id = Some("beef".into());
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        // Deliberately NOT expanded: the shared suite reads both without one.
        let els = folders_elements(&state, &[]);
        let badge = el(&els, "folder-shared-badge").unwrap();
        assert!(
            badge.text.contains("Shared by") && badge.text.contains("alice"),
            "recipient badge should read 'Shared by alice', got {:?}",
            badge.text
        );
        assert!(el(&els, "folder-leave-button").is_some());
        assert!(el(&els, "folder-share-button").is_none());
        assert!(el(&els, "folder-member-item").is_none());
    }

    #[test]
    fn a_member_row_without_a_group_id_withholds_the_leave_affordance() {
        let mut fs = folder("theirs");
        fs.role = Some("member".into());
        fs.mls_group_id = None;
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![fs]));
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-leave-button").is_none());
    }

    #[test]
    fn the_knock_list_paints_one_card_per_pending_share_with_both_responses() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(Vec::new()));
        state.folders.pending_shares = vec![
            PendingShareView {
                inbox_id: 7,
                shared_by_display: "stranger".into(),
            },
            PendingShareView {
                inbox_id: 9,
                shared_by_display: String::new(),
            },
        ];
        let els = folders_elements(&state, &[]);
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "folder-pending-share")
                .count(),
            2
        );
        assert!(
            el(&els, "folder-pending-share")
                .unwrap()
                .text
                .contains("stranger")
        );
        // A fully unstamped cross-nest share falls back to the i18n label rather
        // than rendering an empty "Shared by ".
        let second = els
            .iter()
            .filter(|e| e.id == "folder-pending-share")
            .nth(1)
            .unwrap();
        assert!(second.text.contains("Unknown"), "got {:?}", second.text);
        // Both responses address the DURABLE inbox id, not a list position.
        let accept = el(&els, "folder-share-accept-button").unwrap();
        assert!(matches!(
            accept.role,
            Role::Button(Gesture::Settings(Action::AcceptFolderShare(7)))
        ));
        let decline = el(&els, "folder-share-decline-button").unwrap();
        assert!(matches!(
            decline.role,
            Role::Button(Gesture::Settings(Action::DeclineFolderShare(7)))
        ));
    }

    #[test]
    fn an_empty_knock_list_paints_no_section_at_all() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(Vec::new()));
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-pending-share").is_none());
        assert!(el(&els, "folder-share-accept-button").is_none());
    }

    /// The cap input is the one editable on the row that also COMMITS —
    /// `Role::InputCommit`, so the shared helper's type-then-click works. A
    /// plain `Role::Input` would answer `not actuable` and the cap would be
    /// unwritable.
    #[test]
    fn the_cap_input_commits_on_activation() {
        let state = owner_state_with_roster(vec![member("bb", "bob", "member")]);
        let els = folders_elements(&state, &[]);
        let cap = el(&els, "folder-member-cap-input").unwrap();
        assert!(
            matches!(
                cap.role,
                Role::InputCommit {
                    gesture: Gesture::Settings(Action::SetFolderMemberAccess {
                        row: 0,
                        member: 0,
                        access: None
                    }),
                    ..
                }
            ),
            "cap input must carry its own commit gesture, got {:?}",
            cap.role
        );
    }

    // ── Per-set device activity (`fauna.folders.devices`) ─────────────────

    fn device(
        device_id: &str,
        label: &str,
        change_count: i64,
    ) -> fauna_protocol::folders::FolderDevice {
        fauna_protocol::folders::FolderDevice {
            device_id: device_id.to_string(),
            label: label.to_string(),
            last_change_at: 0,
            change_count,
            extra: Default::default(),
        }
    }

    /// A state with one owner set expanded and `device_activity` read for it —
    /// the device-activity twin of `owner_state_with_roster`.
    fn owner_state_with_device_activity(
        devices: Vec<fauna_protocol::folders::FolderDevice>,
    ) -> SettingsState {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("s")]));
        state.folders.expanded = Some(0);
        state.folders.device_activity = devices;
        state.folders.device_activity_for = Some("s".into());
        state
    }

    /// A completed read confirming zero recorded activity shows the empty-state
    /// line — the "nothing yet" answer, not silence.
    #[test]
    fn no_device_activity_shows_the_empty_state_once_a_read_has_landed() {
        let state = owner_state_with_device_activity(Vec::new());
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-device-activity-item").is_none());
        assert!(
            els.iter()
                .any(|e| e.text == fauna_i18n::strings::devices::NO_DEVICE_ACTIVITY),
            "the empty-state line must render once a read for THIS set has landed"
        );
    }

    /// Before the async read resolves (`device_activity_for` still `None`), the
    /// empty-state line must NOT flash — same discipline as
    /// `an_unshared_owner_row_offers_share_but_paints_no_badge_and_no_members`'s
    /// sibling guard on `roster_for`.
    #[test]
    fn before_a_read_lands_the_empty_state_does_not_flash() {
        let mut state = empty_state();
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("s")]));
        state.folders.expanded = Some(0);
        assert!(state.folders.device_activity_for.is_none());
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-device-activity-item").is_none());
        assert!(
            !els.iter()
                .any(|e| e.text == fauna_i18n::strings::devices::NO_DEVICE_ACTIVITY),
            "must not show 'no activity' before the read for this set resolves"
        );
    }

    /// Each `folder-device-activity-item` carries its `-label`/`-count`
    /// children, scoped `folder-row[i] / folder-device-activity-item[k]` —
    /// the same nested-scope contract `folder-member-item` uses.
    #[test]
    fn device_activity_items_render_label_and_count_scoped_under_the_row() {
        let state = owner_state_with_device_activity(vec![
            device(&"aa".repeat(32), "laptop", 3),
            device(&"bb".repeat(32), "phone", 1),
        ]);
        let els = folders_elements(&state, &[]);

        let items = ids_of(&els, "folder-device-activity-item");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].path, vec![("folder-row".to_string(), 0)]);

        let labels = ids_of(&els, "folder-device-activity-label");
        assert_eq!(labels[0].text, "laptop");
        assert_eq!(labels[1].text, "phone");
        assert_eq!(
            labels[0].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-device-activity-item".to_string(), 0)
            ]
        );

        let counts = ids_of(&els, "folder-device-activity-count");
        assert_eq!(counts[0].text, "3");
        assert_eq!(counts[1].text, "1");
        assert_eq!(
            counts[1].path,
            vec![
                ("folder-row".to_string(), 0),
                ("folder-device-activity-item".to_string(), 1)
            ]
        );
    }

    /// A read for a DIFFERENT set must never paint here — the guard that makes
    /// a fast row-to-row toggle (or a stale in-flight push refresh) safe,
    /// mirroring `a_roster_read_for_another_set_is_not_painted`.
    #[test]
    fn a_device_activity_read_for_another_set_is_not_painted() {
        let mut state =
            owner_state_with_device_activity(vec![device(&"aa".repeat(32), "laptop", 3)]);
        state.folders.device_activity_for = Some("some-other-set".into());
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-device-activity-item").is_none());
    }

    // ── The co-present offline-share affordance (`p2p.md` § Offline share
    // initiation). Every assertion here is about the PAGE's mapping; the
    // decisions themselves are pinned in the shared crate's own tests
    // (`group_ceremony_view`), so these deliberately do not re-assert them.
    #[cfg(feature = "p2p-share")]
    mod offline_share {
        use super::*;
        use crate::offline_share::OfflineShareState;
        use fauna_client_capabilities::group_ceremony_view::{CeremonyStatus, OfflineSharePanel};

        fn available_state() -> SettingsState {
            let mut state = empty_state();
            state.devices.snapshot = Some(snapshot_with_folders(Vec::new()));
            state.offline_share = crate::offline_share::init(&"5c".repeat(32));
            state
        }

        fn peer_code() -> String {
            fauna_core::identity::ActorKeypair::from_secret([0x9Au8; 32])
                .actor_id()
                .to_hex()
        }

        fn invitation(scope: u8) -> crate::offline_share::PendingGroupShareView {
            crate::offline_share::PendingGroupShareView {
                scope_id: [scope; 32],
                initiator: "abc123".into(),
                short_id: format!("{scope:02x}").repeat(4),
            }
        }

        fn scope_view(scope: u8, shared_by: Option<&str>) -> crate::offline_share::GroupScopeView {
            crate::offline_share::GroupScopeView {
                short_id: format!("{scope:02x}").repeat(4),
                member_count: 2,
                shared_by: shared_by.map(str::to_string),
            }
        }

        /// Every element carrying `id`, in paint order.
        fn all<'a>(els: &'a [Element], id: &str) -> Vec<&'a Element> {
            els.iter().filter(|e| e.id == id).collect()
        }

        /// The consent card is the SAME element family as an M2 knock — one
        /// list of things awaiting an answer, as ui.yaml declares the id.
        #[test]
        fn a_group_invitation_knocks_on_the_same_pending_share_family() {
            let mut state = available_state();
            state.folders.group_shares.invitations = vec![invitation(0xA1)];
            let els = folders_elements(&state, &[]);

            let cards = all(&els, "folder-pending-share");
            assert_eq!(cards.len(), 1, "the invitation paints one card");
            assert!(
                cards[0].text.contains("abc123"),
                "the card names who is handing the set over: {:?}",
                cards[0].text
            );
            assert!(
                cards[0].text.contains(&invitation(0xA1).short_id),
                "…and the short scope id both people can see: {:?}",
                cards[0].text
            );
            assert_eq!(all(&els, "folder-share-accept-button").len(), 1);
            assert_eq!(all(&els, "folder-share-decline-button").len(), 1);
        }

        /// The indexed family is SHARED with the M2 knocks, so the ceremony's
        /// cards continue that index rather than restarting it — otherwise
        /// `folder-pending-share[k] / folder-share-accept-button` would
        /// resolve two buttons under one card and none under another.
        #[test]
        fn group_cards_continue_the_m2_knock_index_rather_than_restarting_it() {
            let mut state = available_state();
            state.folders.pending_shares = vec![
                PendingShareView {
                    inbox_id: 11,
                    shared_by_display: "someone".into(),
                },
                PendingShareView {
                    inbox_id: 12,
                    shared_by_display: "someone-else".into(),
                },
            ];
            state.folders.group_shares.invitations = vec![invitation(0xB2), invitation(0xC3)];
            let els = folders_elements(&state, &[]);

            assert_eq!(all(&els, "folder-pending-share").len(), 4, "two of each");
            let scopes: Vec<usize> = all(&els, "folder-share-accept-button")
                .iter()
                .map(|e| e.path[0].1)
                .collect();
            assert_eq!(
                scopes,
                vec![0, 1, 2, 3],
                "each accept sits under its OWN card, M2 then ceremony"
            );
        }

        /// The gestures address the scope, never the row — an offer can land
        /// while the user's finger is moving, and a position-addressed accept
        /// would then consent to the wrong share.
        #[test]
        fn consent_gestures_carry_the_scope_id_not_the_row_position() {
            let mut state = available_state();
            state.folders.group_shares.invitations = vec![invitation(0xD4), invitation(0xE5)];
            let els = folders_elements(&state, &[]);

            let second = all(&els, "folder-share-accept-button")[1];
            match &second.role {
                crate::element::Role::Button(crate::element::Gesture::Settings(
                    Action::AcceptGroupShare(id),
                )) => assert_eq!(*id, [0xE5; 32]),
                other => panic!("the second accept must carry the second scope: {other:?}"),
            }
            let second_decline = all(&els, "folder-share-decline-button")[1];
            match &second_decline.role {
                crate::element::Role::Button(crate::element::Gesture::Settings(
                    Action::DeclineGroupShare(id),
                )) => assert_eq!(*id, [0xE5; 32]),
                other => panic!("the second decline must carry the second scope: {other:?}"),
            }
        }

        /// The success condition, at the render layer: a landed shared
        /// set is a `folder-row` like any other set, badged with who shared it.
        #[test]
        fn a_landed_group_scope_lists_as_a_folder_row_badged_with_its_sharer() {
            let mut state = available_state();
            state.folders.group_shares.scopes = vec![scope_view(0xF6, Some("them123"))];
            let els = folders_elements(&state, &[]);

            let rows = all(&els, "folder-row");
            assert_eq!(rows.len(), 1, "the shared set is listed");
            assert!(
                rows[0].text.contains(&scope_view(0xF6, None).short_id),
                "the nameless set is identified by its short scope id: {:?}",
                rows[0].text
            );
            let badge = all(&els, "folder-shared-badge");
            assert_eq!(badge.len(), 1);
            assert!(
                badge[0].text.contains("them123"),
                "a set someone else minted reads 'Shared by them': {:?}",
                badge[0].text
            );
            assert_eq!(badge[0].path[0].1, 0, "the badge belongs to its own row");
        }

        /// Own scope, other reading: the initiator sees a member count, not
        /// "shared by" themselves.
        #[test]
        fn your_own_scope_is_badged_with_its_member_count() {
            let mut state = available_state();
            state.folders.group_shares.scopes = vec![scope_view(0x17, None)];
            let els = folders_elements(&state, &[]);
            let badge = all(&els, "folder-shared-badge");
            assert_eq!(badge.len(), 1);
            assert!(
                badge[0].text.contains('2'),
                "your own set reads its member count: {:?}",
                badge[0].text
            );
        }

        /// Group rows continue the M2 set list's index for the same reason the
        /// consent cards do — a `folder-row[i]`-scoped read must land on the
        /// row it names.
        #[test]
        fn group_rows_continue_the_set_list_index() {
            let mut state = available_state();
            state.devices.snapshot = Some(snapshot_with_folders(vec![folder("docs")]));
            state.folders.group_shares.scopes = vec![scope_view(0x28, Some("them"))];
            let els = folders_elements(&state, &[]);

            assert_eq!(all(&els, "folder-row").len(), 2, "the M2 set and the scope");
            assert_eq!(
                all(&els, "folder-shared-badge")[0].path[0].1,
                1,
                "the group row is the SECOND row, and its badge says so"
            );
        }

        /// Nothing shared, nothing offered: the ceremony adds no rows at all.
        #[test]
        fn an_empty_ceremony_surface_paints_nothing() {
            let els = folders_elements(&available_state(), &[]);
            assert!(all(&els, "folder-pending-share").is_empty());
            assert!(all(&els, "folder-row").is_empty());
            assert!(all(&els, "folder-shared-badge").is_empty());
        }

        /// The whole section is absent without a usable identity — an
        /// affordance that cannot work must not paint, and a live id in a
        /// build whose plane is dead is the excision trap `payments`
        /// documents.
        #[test]
        fn the_section_is_absent_when_the_affordance_is_unavailable() {
            let mut state = empty_state();
            state.devices.snapshot = Some(snapshot_with_folders(Vec::new()));
            state.offline_share = OfflineShareState::default();
            let els = folders_elements(&state, &[]);
            for id in [
                "offline-share-button",
                "offline-receive-button",
                "offline-share-own-code",
                "offline-share-peer-code-input",
                "offline-share-begin-button",
                "offline-receive-expect-button",
                "offline-share-status",
                "offline-share-cancel-button",
            ] {
                assert!(el(&els, id).is_none(), "{id} must not paint unavailable");
            }
        }

        /// Closed: the two entry buttons, and nothing that belongs to a panel.
        #[test]
        fn the_closed_panel_shows_exactly_the_two_entry_buttons() {
            let els = folders_elements(&available_state(), &[]);
            assert!(el(&els, "offline-share-button").is_some());
            assert!(el(&els, "offline-receive-button").is_some());
            for id in [
                "offline-share-own-code",
                "offline-share-peer-code-input",
                "offline-share-begin-button",
                "offline-receive-expect-button",
                "offline-share-cancel-button",
            ] {
                assert!(el(&els, id).is_none(), "{id} belongs to an open panel");
            }
        }

        /// Each panel paints its own act button and never the other role's —
        /// the two are opposite ends of one ceremony.
        #[test]
        fn each_open_panel_paints_only_its_own_act_button() {
            let mut state = available_state();
            state.offline_share.panel = OfflineSharePanel::Initiate;
            let els = folders_elements(&state, &[]);
            assert!(el(&els, "offline-share-begin-button").is_some());
            assert!(el(&els, "offline-receive-expect-button").is_none());
            // The entry buttons step aside while a panel is open.
            assert!(el(&els, "offline-share-button").is_none());
            assert!(el(&els, "offline-receive-button").is_none());
            // Code, status and the way out are always there.
            assert!(el(&els, "offline-share-own-code").is_some());
            assert!(el(&els, "offline-share-peer-code-input").is_some());
            assert!(el(&els, "offline-share-status").is_some());
            assert!(el(&els, "offline-share-cancel-button").is_some());

            state.offline_share.panel = OfflineSharePanel::Receive;
            let els = folders_elements(&state, &[]);
            assert!(el(&els, "offline-receive-expect-button").is_some());
            assert!(el(&els, "offline-share-begin-button").is_none());
        }

        /// The own code LEADS with the actor key. What the user compares must
        /// be the identity the dial actually proves, or the compare means
        /// nothing at all. (With no seat bound there is no addressing to
        /// append, so here the code is the bare key — the honest answer
        /// before a panel has bound anything.)
        #[test]
        fn the_own_code_element_carries_the_actor_key_verbatim() {
            let mut state = available_state();
            state.offline_share.panel = OfflineSharePanel::Initiate;
            let els = folders_elements(&state, &[]);
            let own = state.offline_share.own.expect("a usable secret");
            assert_eq!(
                el(&els, "offline-share-own-code").unwrap().text,
                own.to_hex()
            );
        }

        /// The act button is gated on a code that parses, and stays gated
        /// while a ceremony is in flight — a second Begin would mint a
        /// competing scope.
        #[test]
        fn the_act_button_is_disabled_without_a_parsed_code_and_while_in_flight() {
            let mut state = available_state();
            state.offline_share.panel = OfflineSharePanel::Initiate;

            // Nothing typed: disabled, and silent about it.
            let els = folders_elements(&state, &[]);
            assert!(!el(&els, "offline-share-begin-button").unwrap().enabled);

            // Garbage typed: still disabled.
            state.offline_share.peer_code_input = "nope".into();
            let els = folders_elements(&state, &[]);
            assert!(!el(&els, "offline-share-begin-button").unwrap().enabled);

            // A real counterpart code: enabled.
            state.offline_share.peer_code_input = peer_code();
            let els = folders_elements(&state, &[]);
            assert!(el(&els, "offline-share-begin-button").unwrap().enabled);

            // ...until a ceremony is running.
            state.offline_share.status = CeremonyStatus::AwaitingConsent;
            let els = folders_elements(&state, &[]);
            assert!(!el(&els, "offline-share-begin-button").unwrap().enabled);
            // Cancel must survive that, or an in-flight ceremony is a trap.
            assert!(el(&els, "offline-share-cancel-button").is_some());
        }

        /// The status element reports the state it is in — pinned because a
        /// status that never changes is indistinguishable from a broken one.
        #[test]
        fn the_status_element_tracks_the_ceremony_state() {
            let mut state = available_state();
            state.offline_share.panel = OfflineSharePanel::Receive;
            let idle = folders_elements(&state, &[]);
            state.offline_share.status = CeremonyStatus::Expecting;
            let expecting = folders_elements(&state, &[]);
            assert_ne!(
                el(&idle, "offline-share-status").unwrap().text,
                el(&expecting, "offline-share-status").unwrap().text
            );
        }
    }

    // ── The post-create device-place editor (`folder-place-row`) ──────────

    fn place_member(
        device_id: &str,
        label: &str,
        flags: fauna_protocol::folders::PlaceFlags,
    ) -> fauna_protocol::folders::FolderMember {
        fauna_protocol::folders::FolderMember {
            device_id: device_id.to_string(),
            label: label.to_string(),
            flags,
            ..Default::default()
        }
    }

    /// The editor renders each device seat's flags under the fresh guard.
    #[test]
    fn device_places_render_flags_under_the_fresh_guard() {
        use fauna_protocol::folders::PlaceFlags;
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("docs")]));

        // Fresh: read FOR this row.
        state.folders.device_places = vec![
            place_member("aa", "laptop", PlaceFlags::archive_place()),
            place_member("bb", "nas", PlaceFlags::new(true, false, true)),
        ];
        state.folders.device_places_for = Some("docs".to_string());

        let els = folders_elements(&state, &[]);
        let rows: Vec<&Element> = els.iter().filter(|e| e.id == "folder-place-row").collect();
        assert_eq!(rows.len(), 2, "every seat gets its row");
        assert_eq!(rows[0].text, "laptop");

        let deletes: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "folder-place-applies-deletes")
            .collect();
        assert_eq!(deletes.len(), 2, "every seat paints its checkboxes");
        assert_eq!(deletes[0].attrs, vec![("state".into(), "off".into())]);
        assert_eq!(deletes[1].attrs, vec![("state".into(), "on".into())]);
        let accepts: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "folder-place-accepts")
            .collect();
        assert_eq!(accepts[0].attrs, vec![("state".into(), "on".into())]);
        assert_eq!(accepts[1].attrs, vec![("state".into(), "off".into())]);

        // The checkboxes scope under their seat's row, which itself scopes
        // under the expanded folder row — the registry's documented
        // `folder-row[i] -> folder-place-row[j]` path, exactly.
        assert_eq!(
            deletes[1].path,
            vec![
                ("folder-row".to_string(), 0usize),
                ("folder-place-row".to_string(), 1usize)
            ],
            "children hang under their seat's row scope, under the folder row"
        );

        // The stale guard: the same list under a DIFFERENT expanded row's name
        // paints nothing.
        state.folders.device_places_for = Some("other".to_string());
        let els = folders_elements(&state, &[]);
        assert!(
            el(&els, "folder-place-row").is_none(),
            "a stale places list must never paint under another row"
        );
    }

    // ── Destination places (`folder-destination-row`) ─────────────────────

    fn destination_place(id: &str, label: &str, attached: bool) -> FolderDestinationPlace {
        FolderDestinationPlace {
            destination_id: id.to_string(),
            label: label.to_string(),
            attached,
            folder_set: attached.then(|| format!("__folder/{}/1", "ab".repeat(32))),
        }
    }

    /// The destination-places section renders attached rows (each with its
    /// detach button scoped under it, under the folder row) and the attach
    /// pair for the rest — under the fresh guard, absent when collapsed, and
    /// with the attach button disabled until a pick is staged.
    #[test]
    fn destination_places_render_under_the_fresh_guard() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("docs")]));
        state.folders.destination_places = vec![
            destination_place("dest-1", "Home nest", true),
            destination_place("dest-2", "Offsite", false),
        ];
        state.folders.destination_places_for = Some("docs".to_string());

        let els = folders_elements(&state, &[]);
        let rows: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "folder-destination-row")
            .collect();
        assert_eq!(rows.len(), 1, "only ATTACHED destinations get a row");
        assert_eq!(rows[0].text, "Home nest");

        // The detach button scopes under its row, under the folder row — the
        // registry's `folder-row[i] -> folder-destination-row[j]` path.
        let detach = el(&els, "folder-destination-detach-button")
            .expect("the attached row carries its detach button");
        assert_eq!(
            detach.path,
            vec![
                ("folder-row".to_string(), 0usize),
                ("folder-destination-row".to_string(), 0usize)
            ]
        );

        // The attach pair paints for the unattached remainder — and the
        // button is disabled until a pick is staged (an empty attach is not a
        // sendable gesture).
        let select = el(&els, "folder-destination-attach-select")
            .expect("an unattached destination offers the attach select");
        assert_eq!(select.text, "");
        let button = el(&els, "folder-destination-attach-button")
            .expect("the attach button paints beside the select");
        assert!(!button.enabled, "no pick staged ⇒ disabled");
        state.folders.destination_attach_selection = "dest-2".to_string();
        let els = folders_elements(&state, &[]);
        assert!(
            el(&els, "folder-destination-attach-button")
                .expect("still painted")
                .enabled,
            "a staged pick enables the attach"
        );

        // The stale guard: a list read for a DIFFERENT row paints nothing.
        state.folders.destination_places_for = Some("other".to_string());
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-destination-row").is_none());
        assert!(el(&els, "folder-destination-attach-select").is_none());

        // Collapsed ⇒ absent entirely, like every expander-body element.
        state.folders.destination_places_for = Some("docs".to_string());
        state.folders.expanded = None;
        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-destination-row").is_none());
    }

    /// An owner with NO enrolled destination sees no section at all — an
    /// affordance that cannot work must not paint (there is nothing to attach
    /// to; enrolling happens on the Backups page).
    #[test]
    fn no_enrolled_destination_paints_no_section() {
        let mut state = empty_state();
        state.folders.expanded = Some(0);
        state.devices.snapshot = Some(snapshot_with_folders(vec![folder("docs")]));
        state.folders.destination_places = Vec::new();
        state.folders.destination_places_for = Some("docs".to_string());

        let els = folders_elements(&state, &[]);
        assert!(el(&els, "folder-destination-row").is_none());
        assert!(el(&els, "folder-destination-attach-select").is_none());
        assert!(el(&els, "folder-destination-attach-button").is_none());
    }
}
