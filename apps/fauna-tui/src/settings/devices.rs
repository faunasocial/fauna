//! The Settings → Devices sub-page (`ui/devices.md`) — the device **roster
//! only**. The 2026-06-28 sync/folder UI unification split the former
//! top-level "Peers" page in two; the folder list / conflicts / creation
//! wizard live on Settings → Folders (`ui/folders.md`, not built here —
//! its own follow-on).
//!
//! A paint shell over the shared `DevicesMachine` (`libs/fauna-devices-machine`),
//! consumed directly like linux's `views/devices_folders/mod.rs` (tui is
//! native Rust, not FFI-mediated — priority #2). Unlike linux's push-observer
//! render loop, this follows the Mail/Privacy sub-page shape already
//! established in this file: an awaited nav-edge hydrate + re-snapshot after
//! every mutating gesture, no live observer tick (`NoopObserver` below is
//! wired but never fires a repaint itself).

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_core::label_custody::LabelCustody;
use fauna_devices_machine::{DevicesMachine, DevicesObserver, DevicesSnapshot, MlsQuery};
use fauna_i18n::strings::devices as t;

use super::Action;
use crate::element::{Element, Gesture};

/// [`DevicesObserver`] is only a construction requirement — `DevicesMachine`
/// notifies it synchronously after every mutation, but this page reads a fresh
/// `snapshot()` right after each awaited `Op` instead (the Mail/Privacy
/// shape), so there is nothing for the callback to do.
struct NoopObserver;

impl DevicesObserver for NoopObserver {
    fn on_changed(&self) {}
}

/// The Devices sub-page's state.
///
/// The machine is built once at the post-auth hook (`attach_session`), the
/// same reasoning as [`super::mail::MailState`]: construction is sync and
/// cheap, and holding it as an `Arc` is what lets an `Op` carry it across a
/// `tokio::spawn`. Session-scoped: `clear_session` drops it.
#[derive(Default)]
pub struct DevicesState {
    /// The shared machine. `None` pre-login.
    pub machine: Option<Arc<DevicesMachine>>,
    /// The last snapshot the page painted. `None` until the nav-edge hydrate
    /// folds one — the roster then renders empty rather than stale.
    pub snapshot: Option<DevicesSnapshot>,
    /// This account's locally-stored device id (hex), read once at
    /// construction — the same value `crate::media::device_id_hex` reads
    /// from the account's `device.db`, cached here rather than re-opened on
    /// every paint.
    /// The **fallback** half of the `device-this-mark-badge` row match
    /// (`devices.md` § This-device marker); [`Self::enrolled_device_row`] is
    /// preferred over it, and `fauna_devices_machine::this_device_row` owns
    /// the rule. `None` pre-registration (the store hasn't minted an id yet,
    /// or it's unreadable) — with no enrolled row either, the badge then
    /// simply never matches, which is the safe default.
    pub(super) local_device_id: Option<String>,
    /// The `sync_devices` row this app **actually enrolled on**
    /// (`AccountStoreHandle::enrolled_device_row`) — the preferred half of the
    /// badge match. It differs from [`Self::local_device_id`] exactly when a
    /// co-located sync agent provisioned by a *different* app on this box
    /// advertises its own id, which decision 2 makes the enrollment target
    /// (`sync-agent.md` § Credential model → the RULED 2026-08-15
    /// block): the app's own id then names no roster row at all. Refreshed on
    /// every Devices hydrate; `None` = nothing enrolled for this actor yet, so
    /// the fallback stands.
    pub(super) enrolled_device_row: Option<String>,
    /// The T16 custody facet (`devices.md` § Custody facet) — the three row
    /// families rendered below the roster. `None` until the nav-edge hydrate
    /// folds one (each family then renders empty rather than stale).
    pub custody: Option<CustodyFacetSnapshot>,
    /// Per-held-row budget drafts (`custody-held-budget-input`), aligned with
    /// `custody.held` and re-seeded at every custody fold — the backups-page
    /// typed-byte-size idiom (`parse_byte_size` at commit).
    pub custody_budget_drafts: Vec<String>,
    /// Whether the offer-initiation (mint) flow is revealed
    /// (`custody-mint-button` → host select + floor + confirm/cancel).
    pub custody_mint_open: bool,
    /// The host candidates the open computed — one per 1:1 conversation
    /// (`fauna_client_conversations::custody_mint_candidates`).
    pub custody_mint_candidates: Vec<fauna_client_conversations::CustodyMintCandidate>,
    /// The chosen candidate index; `None` keeps the confirm disabled.
    pub custody_mint_selected: Option<usize>,
    /// The host's PINNED nest actor identity for the home connection, or
    /// `None` — the consent card's target select renders only with a pin in
    /// hand (the nest-custodian identity fact's no-pin fail-safe). Refreshed
    /// at every custody-facet apply.
    pub custody_nest_pin: Option<[u8; 32]>,
    /// Grants whose consent card has "my nest" picked
    /// (`custody-offer-target-select`) — keyed by grant id, never painted
    /// index (a facet refresh re-orders rows). The accept reads and the act
    /// layer re-validates.
    pub custody_offer_nest_choice: std::collections::HashSet<Vec<u8>>,
    /// Which device principals hold a generation wrap at the observer's
    /// resolved tip (`AccountStoreHandle::keyed_principals`) — the
    /// keyless-posture badge's derived fact. `None` = no resolved tip or no
    /// store this pass: NO badge renders (fail-safe — never a guessed
    /// posture). Refreshed on every Devices hydrate.
    pub keyed_principals: Option<std::collections::BTreeSet<[u8; 32]>>,
    /// The nest's **standing refusal** of this machine's enrollment
    /// (`AccountStoreHandle::enrollment_refusal` — today the tier device cap,
    /// `devices.md` § Step 4), painted on the page's `error-message` while it
    /// stands (`ui/devices.md` § Errors & edge cases). Read fresh on every
    /// Devices hydrate off the shared credential slot, so a refusal the
    /// co-located sync agent's pump met reaches this page even though the app
    /// runs no pump pass of its own. `None` = nothing refused, or healed.
    pub enrollment_refusal: Option<fauna_sync_engine::account_runtime::EnrollmentRefusal>,
    /// The account runtime the machine's fleet-scope removal door reads —
    /// empty until [`wire_fleet_removal`] fills it, and while it is empty the
    /// door refuses every removal ([`FleetRuntimeSlot`]).
    fleet_runtime: FleetRuntimeSlot,
    /// Which `device-member-card` has its removal ARMED — the confirm/cancel
    /// pair painted on that card — the two-step the user chose because the
    /// removal is permanent and the card has no name to recognise the device
    /// by (`ui/devices.md` § Members without a matching entry). Cleared by
    /// cancel, by the confirm's dispatch, and on a fresh visit
    /// (`route_subpage`'s reset, the `sign_out_pending` precedent).
    pub(super) member_remove_pending: Option<String>,
}

/// The custody facet's fold output — the three shared-Rust projections the
/// Devices page renders (owner side, host side, consent), computed by
/// [`load_custody_facet`] off the `fauna.state.custody-ceremony` entries + the registry rows.
///
/// Lives in shared Rust since 2026-08-17: the bundle
/// and its fold moved to `fauna_client_capabilities::view_model` so the six
/// trickle-down legs render from the same projection tui does, rather than
/// each rebuilding it (priority #2). Re-exported under the original path so
/// this page's existing call sites — and `settings::nests`' — are unchanged.
pub use fauna_client_capabilities::view_model::CustodyFacetSnapshot;

/// The custody ACT half — lifted to shared Rust 2026-08-17 as `fauna-client-custody`, the crate that sits above BOTH
/// `fauna-sync-engine` (the R14 (account-data-plane.md § The ratified decisions) registry door) and
/// `fauna-client-conversations` (the ceremony poster) — independent
/// siblings, so no existing crate could host the assembly. Re-exported
/// under the original paths so this module's call sites in
/// `settings::mod` are unchanged.
///
/// Two orderings inside `run_custody_act` are load-bearing and documented
/// there: the accept binds the T10 writer key (never the roster
/// `device.db` id), and revoke hits the nest before recording the signed
/// event.
pub use fauna_client_custody::{CustodyAct, CustodyCtx, load_custody_facet, run_custody_act};

/// Re-seed the per-row budget drafts from a fresh facet — called at every
/// custody fold so the inputs always start from the budget in force.
pub fn seed_budget_drafts(facet: &CustodyFacetSnapshot) -> Vec<String> {
    // The which-value decision is shared (`budget_draft_texts` — the row's
    // live cap, never the accept's); this resolves it against tui's own
    // string table, the same split every other `fauna_core::format` helper
    // uses here.
    fauna_client_capabilities::view_model::budget_draft_texts(facet)
        .into_iter()
        .map(|t| t.resolve(fauna_i18n::strings::lookup))
        .collect()
}

impl DevicesState {
    /// Build the shared machine over `nest`'s authenticated connection. A
    /// build failure is not surfaced here — [`DevicesMachine`] construction is
    /// infallible (unlike Mail, which decodes a secret); kept as a function
    /// for symmetry with the other sub-pages' `attach_session` calls.
    ///
    /// `secret_hex` is the session identity seed, used to wire **label
    /// custody** so the conflict list renders paths sealed-first
    /// (`docs/goal/behavior/file-sync.md` § Sealed names & paths) and the
    /// **foreign-set (cross-nest) list source** — a set shared from ANOTHER
    /// nest has no row in this nest's own list, so `DevicesMachine` unions in
    /// the member's own sealed `fauna.state.folder-keys` records (mirrors linux's
    /// `views/devices_folders/mod.rs` wiring, the `set_mls_query` pattern;
    /// `ui/folders.md` § Implementation status today → *Foreign-set
    /// (cross-nest) list source*). An undecodable secret is not fatal: the
    /// page keeps working, simply falling back to the plaintext render
    /// and no foreign rows — the same fail-safe posture as an app that has not
    /// wired either seam yet.
    ///
    /// `follows` is the account's followed-folders door
    /// ([`super::follows_door`]) — the followed-rows source reads it, and it
    /// needs no secret, so it is wired whatever the secret decodes to.
    pub fn build(
        nest: Arc<NestClient>,
        secret_hex: &str,
        succession_predecessors: &[fauna_core::crypto::BackupKey],
        follows: Arc<dyn fauna_client_config::FollowsStore>,
    ) -> Self {
        let observer: Arc<dyn DevicesObserver> = Arc::new(NoopObserver);
        // One runtime slot for every account-plane door of the page — the fleet
        // door, the participation door and the folder-key custody the create /
        // delete helpers, the foreign-set list and the label resolver read.
        let fleet_runtime = FleetRuntimeSlot::default();
        let folder_keys = crate::settings::folder_key_door(Arc::clone(&fleet_runtime));
        let machine = fauna_devices_machine::build_devices_machine(
            Arc::clone(&nest),
            Some(Arc::clone(&folder_keys)),
            observer,
        );
        // The **followed-folder source** — the account's `fauna.state.follows`
        // rows, each probed for availability. The probes ride a staleness
        // budget + bounded concurrency (`public_follow::resolve_availability`),
        // so wiring it puts no cross-nest round trip per followed folder on
        // every refresh — see `folders.md` § Publicly-synced follow.
        machine.set_followed_folders_source(Arc::new(
            fauna_devices_machine::StoreFollowedFoldersSource::new(Arc::clone(&nest), follows),
        ));
        machine.set_fleet_removal(fleet_removal_door(Arc::clone(&fleet_runtime)));
        machine.set_p2p_participation_door(p2p_participation_door(Arc::clone(&fleet_runtime)));
        let secret = decode_secret(secret_hex);
        match secret {
            Some(secret) => {
                machine.set_foreign_sets_source(Arc::new(
                    fauna_devices_machine::CustodyForeignSetsSource::new(
                        Arc::clone(&folder_keys) as Arc<dyn fauna_client_folders::FolderKeyReader>
                    ),
                ));
                machine.set_label_custody(label_custody(
                    nest,
                    secret,
                    folder_keys,
                    succession_predecessors,
                ))
            }
            None => tracing::warn!(
                "[settings/devices] undecodable identity secret — conflict paths render \
                 from the plaintext only, and cross-nest shared sets stay unlisted"
            ),
        }
        // This account's id — the one its sync agent registers under, so
        // the this-device marker lands on this account's own row.
        let local_device_id = secret.and_then(crate::media::device_id_hex_for_secret);
        // The participation rule's last fallback: before the runtime names
        // the enrolled row (or the fleet id), the app's own id is the row the
        // machine paints and acts on as this device's (`p2p.md` § Per-device
        // participation → *Which row is this device's*).
        machine.set_this_device_row(local_device_id.clone());
        DevicesState {
            machine: Some(machine),
            snapshot: None,
            local_device_id,
            fleet_runtime,
            ..Default::default()
        }
    }
}

/// The tui impl of the B3 member-row join-filter seam ([`MlsQuery`]) — the
/// per-app glue the centralized `DevicesMachine` filter delegates to. `DevicesMachine::refresh` drops
/// every `role == "member"` row this answers `false` for, so a
/// rostered-but-un-joined knock never reaches the snapshot the Folders page
/// renders — a stranger's share surfaces ONLY as a `folder-pending-share`
/// (`ui/folders.md` § Sharing: "a stranger cannot force a set into your list").
///
/// ⚠ **Until this is wired the machine is fail-safe, which means EVERY member row
/// is dropped** (`libs/fauna-devices-machine/src/machine.rs:141`) — so a set
/// legitimately shared *with* this user is invisible, badge and leave button
/// included, no matter what the page paints. tui shipped un-wired from the core
/// control plane (2026-07-22) until 2026-07-30; the recipient half of the sharing
/// slice could not work without it.
///
/// Reads the ONE live per-actor `MlsEngine` off the conversations rail
/// (`ConversationsSession::engine`, over the single `mls_state.db`) — never a
/// second engine racing the SQLite file. Unlike linux's `LinuxMlsQuery`, which
/// re-resolves a process-global `active_session()` on every call, tui holds the
/// session `Arc` directly: `session.rs` starts the conversations session *before*
/// `settings.attach_session`, so the handle exists by wiring time, and a re-login
/// rebuilds both. Derives the set's `ChannelId` the SAME way the nest does
/// (`ChannelId::from_group_id` over a plain-`hex` decode — openMLS group ids are
/// variable length, so NOT `hex32`; see `folders::channel_id_from_group_id_hex`).
struct TuiMlsQuery {
    session: Arc<fauna_conversations::ConversationsSession>,
}

impl MlsQuery for TuiMlsQuery {
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool {
        let Ok(channel_id) = super::folders::channel_id_from_group_id_hex(mls_group_id_hex) else {
            return false;
        };
        self.session.engine().has_group(&channel_id)
    }
}

/// Wire the B3 join-filter once the conversations session is live. Called from
/// `session.rs`'s post-auth hook right after `attach_session`, because the
/// session handle lives on `App::conversations` rather than on `SettingsState`.
/// A no-op when either half is missing — the machine then keeps its fail-safe
/// default (no member rows), which is the correct direction.
pub fn wire_mls_query(
    state: &DevicesState,
    session: Option<&Arc<fauna_conversations::ConversationsSession>>,
) {
    let (Some(machine), Some(session)) = (state.machine.as_ref(), session) else {
        tracing::warn!(
            "[settings/devices] no MLS join-filter wired — sets shared WITH this user \
             stay hidden (DevicesMachine fail-safe)"
        );
        return;
    };
    machine.set_mls_query(Arc::new(TuiMlsQuery {
        session: Arc::clone(session),
    }));
}

/// Where tui's fleet-scope removal door
/// ([`fauna_devices_machine::FleetRemoval`]) reads its runtime — the devices
/// page's remove-device action's second leg beside `fauna.sync.devices.delete`
/// (`docs/goal/behavior/devices.md` § Removing a Device;
/// `docs/goal/architecture/account-data-taxonomy.md` § The generation
/// machinery → *Fleet-scope reclamation*, clause (4)).
///
/// The door itself is the shared adapter every runtime-hosting seat wires
/// (`fauna_client_account_runtime::fleet_removal`, which owns the rule that an
/// absent runtime refuses the removal; web's core chunk serves the same
/// adapter through the account port). What is tui's own is only where the
/// handle lives: [`FleetRuntimeSlot`], `App`-owned state rather than the
/// process static linux and the `fauna-ffi` seat read (this app's no-globals
/// shape — `SettingsState::account_store_assembly`'s docs). **The door is
/// wired when the machine is built, never at the account-store-ready edge**:
/// wiring it there left the machine door-less until the assembly settled —
/// and for the whole session when it failed — and a door-less machine deletes
/// the nest row alone — a removal no runtime-hosting app may make.
type FleetRuntimeSlot =
    Arc<std::sync::Mutex<Option<fauna_sync_engine::account_runtime::AccountStoreHandle>>>;

fn fleet_removal_door(slot: FleetRuntimeSlot) -> Arc<dyn fauna_devices_machine::FleetRemoval> {
    Arc::new(
        fauna_client_account_runtime::fleet_removal::RuntimeFleetRemoval::new(move || {
            slot.lock().ok().and_then(|handle| handle.clone())
        }),
    )
}

/// This device's own peer-participation door (`device-p2p-participation-toggle`
/// on the own row; `p2p.md` § Per-device participation) — the shared impl over
/// the SAME runtime slot the removal door reads, so both answer from one
/// handle and both refuse while there is none. When the co-located agent
/// holds the engine, the switch asks it for its pass through the seat's
/// holder nudge, which `sync_agent` publishes with its provisioner.
fn p2p_participation_door(
    slot: FleetRuntimeSlot,
) -> Arc<dyn fauna_devices_machine::P2pParticipation> {
    use fauna_client_account_runtime::p2p_participation::{
        EngineHolderNudgeSlot, RuntimeP2pParticipation,
    };
    Arc::new(
        RuntimeP2pParticipation::new(move || slot.lock().ok().and_then(|handle| handle.clone()))
            .with_holder_nudge(EngineHolderNudgeSlot::seat()),
    )
}

/// Hand the fleet-scope removal door its runtime once the account-store
/// handle lands (`UiMessage::Data(DataMessage::AccountStoreReady)` in
/// `app.rs`, beside `wire_account_store_seams`) — and again whenever the
/// machine is rebuilt under a runtime that is already up. Until then the door
/// refuses every removal.
pub fn wire_fleet_removal(
    state: &DevicesState,
    handle: fauna_sync_engine::account_runtime::AccountStoreHandle,
) {
    if let Ok(mut slot) = state.fleet_runtime.lock() {
        *slot = Some(handle);
    }
}

/// The session identity seed as raw bytes, or `None` if it will not decode.
fn decode_secret(secret_hex: &str) -> Option<[u8; 32]> {
    fauna_core::hex32::decode(secret_hex).ok()
}

/// The reader's label-opening custody for the conflict list: the owner backup
/// key plus the shared-folder content-key resolver, so a conflict on a set
/// **shared with** this user renders under the set's content keys rather than
/// silently omitting.
///
/// The same `NestFolderKeyResolver` the Media page builds over the same
/// custody — one resolver, because whoever can open a set's bytes renders its
/// names (`docs/goal/behavior/file-sync.md` § Sealed names & paths).
fn label_custody(
    nest: Arc<NestClient>,
    secret: [u8; 32],
    folder_keys: Arc<dyn fauna_client_folders::FolderKeyStore>,
    succession_predecessors: &[fauna_core::crypto::BackupKey],
) -> LabelCustody {
    let resolver: Arc<dyn fauna_core::folder_keys::FolderKeyResolver> = Arc::new(
        fauna_client_folders::NestFolderKeyResolver::new(nest, folder_keys),
    );
    LabelCustody::new(
        Some(resolver),
        Some(fauna_core::crypto::BackupKey::derive(&secret)),
    )
    // Read candidates for the rows a succession re-pointed but did not re-seal;
    // never a seal root (`LabelCustody::with_predecessors`).
    .with_predecessors(succession_predecessors.to_vec())
}

/// The ordered ui.yaml `devices` element list — the roster only. `device-card`
/// is the per-row anchor; every row child (`device-name`/`device-status`/
/// `device-guardian-mark-badge`/`device-this-mark-badge`/
/// `device-remove-button`) is `.within(ids::DEVICE_CARD, i)` — the `settings.rs`
/// anchor-plus-`.within` idiom. `peer-actor-id-copy-btn` is the one exception:
/// a single page-level instance, unscoped, rendered once before the roster
/// loop (ui.yaml `indexed: false` — see the button's own doc comment below).
///
/// The children stay readable FLAT as well (an empty scope resolves to the
/// whole frame, so an unscoped query matches every row in registration
/// order) — which is how most of the suite
/// drives them. But the scope is load-bearing for the guardian marker: the ward
/// must be able to tell WHICH device their guardian enrolled
/// (`family-safety.md` § Full visibility for young children), and
/// `test_family.py`'s Slice-F test asserts exactly that, reading
/// `device-guardian-mark-badge` under `device-card[i]`. Unscoped, that read
/// found nothing on the marked card and "found nothing" on the unmarked one —
/// a false pass on the negative half.
pub(super) fn devices_elements(state: &DevicesState) -> Vec<Element> {
    let mut els = vec![Element::label(ids::PAGE_HEADING, t::TITLE)];
    // Single page-level instance (ui.yaml `indexed: false`, `devices.md` §
    // Layout & flow point 2) — copies THIS client's own actor ID, for handing
    // to a new device being paired. Rendered unconditionally (even with an
    // empty roster, since pairing the first device is exactly when this is
    // needed), never scoped under a `device-card`. `Action::CopyActorId`
    // reads `state.account_actor_id` at dispatch time, the same action the
    // Account sub-page's `account-actor-id-copy-btn` already uses.
    els.push(Element::gesture_button(
        ids::PEER_ACTOR_ID_COPY_BTN,
        t::COPY_ACTOR_ID,
        true,
        Gesture::Settings(Action::CopyActorId),
    ));
    let devices = state
        .snapshot
        .as_ref()
        .map(|s| s.devices.as_slice())
        .unwrap_or(&[]);
    // The row `device-this-mark-badge` marks — resolved once for the whole
    // roster, not per row. The marked value is the row the app ENROLLED on,
    // not the id it happens to hold locally; `this_device_row` owns that rule
    // and the reason the fallback is load-bearing.
    let this_row = fauna_devices_machine::this_device_row(
        state.enrolled_device_row.as_deref(),
        state.local_device_id.as_deref(),
    );
    for (i, device) in devices.iter().enumerate() {
        els.push(Element::label(ids::DEVICE_CARD, String::new()));
        els.push(
            Element::label(ids::DEVICE_NAME, device.label.clone()).within(ids::DEVICE_CARD, i),
        );
        els.push(
            Element::label(
                ids::DEVICE_STATUS,
                fauna_core::format::device_status_label(device.online)
                    .resolve(fauna_i18n::strings::lookup),
            )
            .within(ids::DEVICE_CARD, i),
        );
        if device.guardian_marked {
            els.push(
                Element::label(ids::DEVICE_GUARDIAN_MARK_BADGE, t::GUARDIAN_MARKED_BADGE)
                    .within(ids::DEVICE_CARD, i),
            );
        }
        // Not mutually exclusive with the guardian badge above — a guardian
        // marking their own enrolled device can legitimately carry both
        // (`devices.md` § This-device marker).
        if this_row.as_deref() == Some(device.device_id.as_str()) {
            els.push(
                Element::label(ids::DEVICE_THIS_MARK_BADGE, t::THIS_DEVICE_BADGE)
                    .within(ids::DEVICE_CARD, i),
            );
            // This device's own key fingerprint, beside the marker — the
            // user's half of the member group's comparison, rendered by the
            // shared machine through the SAME formatter every
            // `device-member-fingerprint` uses (`ui/devices.md` § Members
            // without a matching entry). Absent until the runtime answered.
            if let Some(fingerprint) = state
                .snapshot
                .as_ref()
                .and_then(|s| s.own_fingerprint.as_deref())
            {
                els.push(
                    Element::label(
                        ids::DEVICE_OWN_FINGERPRINT,
                        fauna_core::localized::LocalizedText::key_arg(
                            "devices.own_fingerprint",
                            "fingerprint",
                            fingerprint.to_string(),
                        )
                        .resolve(fauna_i18n::strings::lookup),
                    )
                    .within(ids::DEVICE_CARD, i),
                );
            }
        }
        // The keyless-posture marker (`devices.md` § Custody facet piece 1):
        // DERIVED bundle-key reach, joined and fail-safed by the shared
        // `fauna_devices_machine::keyless_posture` rule every app reads.
        // Posture is never stored or asked — no toggle, only the fact.
        if fauna_devices_machine::keyless_posture(
            state.keyed_principals.as_ref(),
            device.principal.as_deref(),
        ) {
            els.push(
                Element::label(ids::DEVICE_KEYLESS_POSTURE_BADGE, t::KEYLESS_POSTURE_BADGE)
                    .within(ids::DEVICE_CARD, i),
            );
        }
        // One `device-folder-role-badge` chip per folder this device
        // carries, stating its place in that set (`DeviceSummary.folders`,
        // the roster slice's own field) — composed from the SAME place labels
        // the create wizard's checkboxes carry
        // (`fauna_core::format::device_place_label`, nested resolve).
        // Same multi-per-scope shape as `tag-chip`/`protocol-badge` in
        // `feed/mod.rs`: several entries share one `.within(ids::DEVICE_CARD, i)`.
        // Reference: apple `DevicesContent.swift`'s `RoleBadge` row
        // (`devices.md` § Element table — `device-folder-role-badge`).
        for fs in &device.folders {
            els.push(
                Element::label(
                    ids::DEVICE_FOLDER_ROLE_BADGE,
                    fauna_core::format::device_place_label(
                        fs.originates,
                        fs.accepts,
                        fs.applies_deletes,
                    )
                    .resolve_nested(fauna_i18n::strings::lookup),
                )
                .within(ids::DEVICE_CARD, i),
            );
        }
        // `device-p2p-participation-toggle` (`p2p.md` § Per-device
        // participation — rule 5's off switch; ID user-approved 2026-09-25).
        // Painted as the shared machine published it — own-ness, checked,
        // label, actionable — decided by the same own-row rule its gesture
        // takes its arm by, so the app never re-derives which row is its own.
        let paint = device.participation_paint();
        els.push(
            Element::checkbox_gesture(
                ids::DEVICE_P2P_PARTICIPATION_TOGGLE,
                paint.label.resolve(fauna_i18n::strings::lookup),
                paint.checked,
                Gesture::Settings(Action::SetP2pParticipation {
                    index: i as u32,
                    on: !paint.checked,
                }),
            )
            .enabled(paint.actionable)
            .within(ids::DEVICE_CARD, i),
        );
        els.push(
            Element::gesture_button(
                ids::DEVICE_REMOVE_BUTTON,
                fauna_i18n::strings::common::DELETE,
                true,
                Gesture::Settings(Action::RemoveDevice(i as u32)),
            )
            .within(ids::DEVICE_CARD, i),
        );
    }
    // ── Signed-in devices without a matching entry — the member group,
    // below the roster and before the custody families (`devices.md` §
    // Members without a matching entry).
    member_elements(state, &mut els);
    // ── The T16 custody facet (`devices.md` § Custody facet) — three row
    // families below the roster, distinctly-labeled groups never intermixed
    // with `device-card`. Copy is trust vocabulary only; the three receipt
    // states are three different strings by spec, never collapsed.
    custody_elements(state, &mut els);
    // `settings-nav-back` — Esc already returns to the Settings hub
    // (`Action::NavBack => state.sub = SubPage::Root`), but a live user report
    // found it was the ONLY way out on Folders, undiscoverable (user-approved
    // 2026-08-03; matches `account.rs`/`folders.rs`'s existing pattern).
    els.push(
        Element::gesture_button(
            ids::SETTINGS_NAV_BACK,
            fauna_i18n::strings::common::BACK,
            true,
            Gesture::Settings(Action::NavBack),
        )
        .nav_back(),
    );
    els
}

/// The receipt-status line — three states, three strings (the A7 honesty
/// rule: fresh / stale / no-receipt-yet never collapse or go empty).
///
/// The resolve (label + `{when}` substitution) moved into
/// `fauna-client-capabilities` — it was byte-identical to linux's twin. This is now a bare
/// lookup-table forward.
pub(super) fn receipt_status_text(
    state: fauna_client_capabilities::view_model::ReceiptState,
    attested_at_micros: Option<u64>,
) -> String {
    fauna_client_capabilities::view_model::custody_receipt_status_text(
        state,
        attested_at_micros,
        fauna_i18n::strings::lookup,
    )
}

/// The held-bytes-against-budget line, with the degraded marker riding it
/// when the receipt reports evicted or capped-short coverage (degraded is
/// orthogonal to freshness — a fresh receipt can honestly say "I dropped
/// payload").
///
/// The resolve moved into `fauna-client-capabilities` alongside its
/// receipt-status sibling — it was already byte-identical to linux's twin once both apps
/// shared one string table.
pub(super) fn held_bytes_text(
    receipt: Option<&fauna_client_capabilities::view_model::CustodyReceiptView>,
) -> String {
    fauna_client_capabilities::view_model::custody_held_bytes_text(
        receipt,
        fauna_i18n::strings::lookup,
    )
}

pub(super) fn short_actor(id: &[u8; 32]) -> String {
    fauna_core::format::short_id(&hex::encode(id))
}

/// The signed-in devices without a matching entry (`ui/devices.md` § Members
/// without a matching entry; `account-data-taxonomy.md` § The generation
/// machinery → *Fleet-scope reclamation*, clause (4), *A disagreement is the
/// user's to settle*) — a separately-labelled group below the roster, never
/// intermixed with `device-card` rows: one `device-member-card` per verified
/// fleet member no roster row accounts for (`DevicesSnapshot::members`, the
/// shared derivation and the shared fingerprint render), its claimed sign-in
/// time, and the two-step remove the user chose. Renders NOTHING with no
/// members — a settled honest fleet lists nobody, so the title and the note
/// never paint over an empty group. Every child registers `.within(card, i)`.
fn member_elements(state: &DevicesState, els: &mut Vec<Element>) {
    let members = state
        .snapshot
        .as_ref()
        .map(|s| s.members.as_slice())
        .unwrap_or(&[]);
    if members.is_empty() {
        return;
    }
    // The group title has no ui.yaml id of its own (the approved set names
    // the note and the cards) — painted chrome, like every other untagged
    // heading (`Element::chrome`'s own doc).
    els.push(Element::chrome(t::MEMBERS_TITLE));
    els.push(Element::label(ids::DEVICE_MEMBER_NOTE, t::MEMBER_NOTE));
    for (i, member) in members.iter().enumerate() {
        els.push(Element::label(ids::DEVICE_MEMBER_CARD, String::new()));
        els.push(
            Element::label(
                ids::DEVICE_MEMBER_FINGERPRINT,
                fauna_core::localized::LocalizedText::key_arg(
                    "devices.member_fingerprint",
                    "fingerprint",
                    member.fingerprint.clone(),
                )
                .resolve(fauna_i18n::strings::lookup),
            )
            .within(ids::DEVICE_MEMBER_CARD, i),
        );
        // The member's own self-signed word about when it signed in — a hint,
        // never proof (the snapshot carries the instant; the app formats it
        // locally, the custody receipt line's precedent).
        els.push(
            Element::label(
                ids::DEVICE_MEMBER_ENROLLED_AT,
                fauna_core::localized::LocalizedText::key_arg(
                    "devices.member_enrolled_at",
                    "when",
                    fauna_core::format::format_unix_local_ms(member.enrolled_at_ms),
                )
                .resolve(fauna_i18n::strings::lookup),
            )
            .within(ids::DEVICE_MEMBER_CARD, i),
        );
        if state.member_remove_pending.as_deref() == Some(member.device_id.as_str()) {
            els.push(
                Element::gesture_button(
                    ids::DEVICE_MEMBER_REMOVE_CONFIRM_BUTTON,
                    t::MEMBER_REMOVE_CONFIRM,
                    true,
                    Gesture::Settings(Action::ConfirmRemoveMember(member.device_id.clone())),
                )
                .within(ids::DEVICE_MEMBER_CARD, i),
            );
            els.push(
                Element::gesture_button(
                    ids::DEVICE_MEMBER_REMOVE_CANCEL_BUTTON,
                    fauna_i18n::strings::common::CANCEL,
                    true,
                    Gesture::Settings(Action::CancelRemoveMember),
                )
                .within(ids::DEVICE_MEMBER_CARD, i),
            );
        } else {
            els.push(
                Element::gesture_button(
                    ids::DEVICE_MEMBER_REMOVE_BUTTON,
                    fauna_i18n::strings::common::REMOVE,
                    true,
                    Gesture::Settings(Action::RemoveMember(member.device_id.clone())),
                )
                .within(ids::DEVICE_MEMBER_CARD, i),
            );
        }
    }
}

/// The custody facet's three row families (T16). Every child registers
/// `.within(card, i)` — the roster's anchor-plus-`within` idiom.
fn custody_elements(state: &DevicesState, els: &mut Vec<Element>) {
    let Some(facet) = state.custody.as_ref() else {
        return;
    };

    // (1) Owner side — "who holds my data": one card per cross-account
    // custodian device. The card's own line carries the REQUIRED honest-bound
    // revoke copy (nests.md § Trust facet — custody rows), so the bound is
    // stated beside the control it bounds.
    let mut card = 0usize;
    for (i, row) in facet.rows.iter().enumerate() {
        // The render split (the nest-custodian identity fact): a
        // nest-anchored custody belongs to the Nests page's
        // nest-trust-custody-* family — one custody never renders in both.
        // The gesture keeps the ORIGINAL row index (the handler resolves
        // facet.rows[i]); only the card's within-index is dense.
        if row.custodian_nest_url.is_some() {
            continue;
        }
        let bound_note = if row.pending {
            String::new()
        } else {
            t::CUSTODY_REVOKE_BOUND_NOTE.to_string()
        };
        els.push(Element::label(ids::CUSTODY_HOLDER_CARD, bound_note));
        els.push(
            Element::label(ids::CUSTODY_HOLDER_NAME, short_actor(&row.host))
                .within(ids::CUSTODY_HOLDER_CARD, card),
        );
        els.push(
            Element::label(
                ids::CUSTODY_HOLDER_RECEIPT_STATUS,
                receipt_status_text(
                    row.receipt_state,
                    row.receipt.as_ref().map(|r| r.attested_at_micros),
                ),
            )
            .within(ids::CUSTODY_HOLDER_CARD, card),
        );
        els.push(
            Element::label(
                ids::CUSTODY_HOLDER_HELD_BYTES,
                held_bytes_text(row.receipt.as_ref()),
            )
            .within(ids::CUSTODY_HOLDER_CARD, card),
        );
        els.push(
            Element::gesture_button(
                ids::CUSTODY_HOLDER_REVOKE_BUTTON,
                t::CUSTODY_REVOKE,
                // A pending ceremony has minted nothing to revoke yet.
                !row.pending,
                Gesture::Settings(Action::CustodyRevoke(i as u32)),
            )
            .within(ids::CUSTODY_HOLDER_CARD, card),
        );
        card += 1;
    }

    // (2) Host side — "what I hold for others": one card per accepted
    // custody, with the adjustable budget (the row's live value) and the
    // always-available stop control (disabled once stopped — the row then
    // honestly shows the hold has ended).
    for (i, held) in facet.held.iter().enumerate() {
        els.push(Element::label(ids::CUSTODY_HELD_CARD, String::new()));
        els.push(
            Element::label(
                ids::CUSTODY_HELD_OWNER,
                t::custody_held_owner(&short_actor(&held.owner)),
            )
            .within(ids::CUSTODY_HELD_CARD, i),
        );
        let scope_text = match &held.scopes {
            Some(fauna_core::custody_grant::CustodyScopeSet::Scopes(list)) => list.join(", "),
            _ => t::CUSTODY_HELD_SCOPE_ACCOUNT.to_string(),
        };
        els.push(
            Element::label(ids::CUSTODY_HELD_SCOPE, scope_text).within(ids::CUSTODY_HELD_CARD, i),
        );
        els.push(
            Element::label(
                ids::CUSTODY_HELD_BYTES,
                held_bytes_text(held.receipt.as_ref()),
            )
            .within(ids::CUSTODY_HELD_CARD, i),
        );
        let draft = state
            .custody_budget_drafts
            .get(i)
            .cloned()
            .unwrap_or_else(|| crate::format::byte_size(held.retained_bytes_cap as i64));
        els.push(
            Element::input_commit(
                ids::CUSTODY_HELD_BUDGET_INPUT,
                draft,
                crate::element::Field::Settings(super::SettingsField::CustodyBudget(i as u32)),
                Gesture::Settings(Action::CustodySetBudget(i as u32)),
            )
            .labelled(t::CUSTODY_BUDGET_LABEL)
            .within(ids::CUSTODY_HELD_CARD, i),
        );
        els.push(
            Element::gesture_button(
                ids::CUSTODY_HELD_STOP_BUTTON,
                // Stopping pauses the hold; it does not free the space. Once
                // stopped, the control's label carries that honest half (row
                // 67) — the bytes stay until the custody is removed.
                if held.stopped {
                    t::CUSTODY_STOPPED_BYTES_REMAIN
                } else {
                    t::CUSTODY_STOP
                },
                !held.stopped,
                Gesture::Settings(Action::CustodyStopHolding(i as u32)),
            )
            .within(ids::CUSTODY_HELD_CARD, i),
        );
        els.push(
            Element::gesture_button(
                ids::CUSTODY_HELD_REMOVE_BUTTON,
                t::CUSTODY_REMOVE,
                // Always available, stopped or not: stop is the pause and this
                // is the reclaim, so a host who paused first must still be able
                // to get the space back.
                true,
                Gesture::Settings(Action::CustodyRemoveHolding(i as u32)),
            )
            .within(ids::CUSTODY_HELD_CARD, i),
        );
    }

    // (3) Incoming offers — the consent surface. The floor copy is REQUIRED
    // before accept (devices.md § Custody facet piece 3): what this device
    // would see — the shape, never the content.
    for (i, offer) in facet.offers.iter().enumerate() {
        els.push(Element::label(
            ids::CUSTODY_OFFER_CARD,
            t::custody_offer_title(&short_actor(&offer.owner)),
        ));
        els.push(
            Element::label(ids::CUSTODY_OFFER_FLOOR_NOTE, t::CUSTODY_OFFER_FLOOR)
                .within(ids::CUSTODY_OFFER_CARD, i),
        );
        // The host-side choice (the nest-custodian identity fact): rendered
        // ONLY for an offer a nest can hold, with a pinned nest identity in
        // hand — absent otherwise, never disabled.
        if offer.nest_can_hold && state.custody_nest_pin.is_some() {
            let on_nest = state.custody_offer_nest_choice.contains(&offer.grant_id);
            let selected = if on_nest {
                t::CUSTODY_OFFER_TARGET_NEST
            } else {
                t::CUSTODY_OFFER_TARGET_DEVICE
            };
            els.push(
                Element::select(
                    ids::CUSTODY_OFFER_TARGET_SELECT,
                    selected,
                    crate::element::SelectTarget::CustodyOfferTarget(i as u32),
                    vec![
                        t::CUSTODY_OFFER_TARGET_DEVICE.to_string(),
                        t::CUSTODY_OFFER_TARGET_NEST.to_string(),
                    ],
                )
                .labelled(t::CUSTODY_OFFER_TARGET_LABEL)
                .within(ids::CUSTODY_OFFER_CARD, i),
            );
        }
        els.push(
            Element::gesture_button(
                ids::CUSTODY_OFFER_ACCEPT_BUTTON,
                t::CUSTODY_OFFER_ACCEPT,
                true,
                Gesture::Settings(Action::CustodyAcceptOffer(i as u32)),
            )
            .within(ids::CUSTODY_OFFER_CARD, i),
        );
        els.push(
            Element::gesture_button(
                ids::CUSTODY_OFFER_DECLINE_BUTTON,
                t::CUSTODY_OFFER_DECLINE,
                true,
                Gesture::Settings(Action::CustodyDeclineOffer(i as u32)),
            )
            .within(ids::CUSTODY_OFFER_CARD, i),
        );
    }

    // (4) Offer initiation — the mint flow (IDs user-approved 2026-08-17).
    // The mint IS the ceremony: v1 offers the Account scope with the default
    // window over an EXISTING 1:1 conversation, so the host picker's options
    // are the 1:1 conversations. The REQUIRED floor copy renders before the
    // confirm (nests.md § Trust facet — custody rows).
    els.push(Element::gesture_button(
        ids::CUSTODY_MINT_BUTTON,
        t::CUSTODY_MINT_BUTTON,
        true,
        Gesture::Settings(Action::CustodyMintOpen),
    ));
    if state.custody_mint_open {
        let options: Vec<String> = state
            .custody_mint_candidates
            .iter()
            .map(|c| c.label.clone())
            .collect();
        let selected = state
            .custody_mint_selected
            .and_then(|i| state.custody_mint_candidates.get(i))
            .map(|c| c.label.clone())
            .unwrap_or_else(|| t::CUSTODY_MINT_HOST_PLACEHOLDER.to_string());
        els.push(
            Element::select(
                ids::CUSTODY_MINT_HOST_SELECT,
                selected,
                crate::element::SelectTarget::CustodyMintHost,
                options,
            )
            .labelled(t::CUSTODY_MINT_HOST_LABEL),
        );
        els.push(Element::label(
            ids::CUSTODY_MINT_FLOOR_NOTE,
            t::CUSTODY_MINT_FLOOR,
        ));
        els.push(Element::gesture_button(
            ids::CUSTODY_MINT_CONFIRM_BUTTON,
            t::CUSTODY_MINT_CONFIRM,
            state.custody_mint_selected.is_some(),
            Gesture::Settings(Action::CustodyMintConfirm),
        ));
        els.push(Element::gesture_button(
            ids::CUSTODY_MINT_CANCEL_BUTTON,
            fauna_i18n::strings::common::CANCEL,
            true,
            Gesture::Settings(Action::CustodyMintCancel),
        ));
    }
}

/// The painted-families fixture the walk (`crate::walk`) and this file's
/// render test share: all three T16 families in their distinct states — a
/// live degraded-receipt owner row + a pending one, a running hold + a
/// stopped one, and one pending offer.
#[cfg(test)]
pub(crate) fn custody_walk_facet() -> CustodyFacetSnapshot {
    use fauna_client_capabilities::view_model::{
        CustodyOfferView, CustodyReceiptView, CustodyRowView, GrantLiveness, HeldCustodyView,
        ReceiptState,
    };
    use fauna_core::custody_grant::CustodyScopeSet;
    CustodyFacetSnapshot {
        rows: vec![
            CustodyRowView {
                grant_id: vec![0x11; 16],
                host: [0xB0; 32],
                custodian_key: Some([0xC5; 32]),
                scopes: Some(CustodyScopeSet::Account),
                lasts_until: Some(1_000_000),
                liveness: Some(GrantLiveness::Active),
                receipt: Some(CustodyReceiptView {
                    held_bytes: 700,
                    retained_bytes_cap: 4_096,
                    degraded: true,
                    attested_at_micros: 5_000_000,
                }),
                receipt_state: ReceiptState::Fresh,
                pending: false,
                custodian_nest_url: None,
            },
            CustodyRowView {
                grant_id: vec![0x22; 16],
                host: [0xB1; 32],
                custodian_key: None,
                scopes: Some(CustodyScopeSet::Account),
                lasts_until: None,
                liveness: None,
                receipt: None,
                receipt_state: ReceiptState::NoReceiptYet,
                pending: true,
                custodian_nest_url: None,
            },
            // A nest-anchored custody (the nest-custodian identity fact):
            // renders on the NESTS page's nest-trust-custody-* family and
            // must stay OFF the Devices custody-holder group — both walks
            // assert their half of the split from this one snapshot.
            CustodyRowView {
                grant_id: vec![0x44; 16],
                host: [0xB2; 32],
                custodian_key: Some([0xAB; 32]),
                scopes: Some(CustodyScopeSet::Account),
                lasts_until: Some(1_000_000),
                liveness: Some(GrantLiveness::Active),
                receipt: None,
                receipt_state: ReceiptState::NoReceiptYet,
                pending: false,
                custodian_nest_url: Some("https://friend-nest.example/".into()),
            },
        ],
        held: vec![
            HeldCustodyView {
                grant_id: vec![0x33; 16],
                owner: [0xA0; 32],
                scopes: Some(CustodyScopeSet::Account),
                retained_bytes_cap: 8_192,
                receipt: Some(CustodyReceiptView {
                    held_bytes: 300,
                    retained_bytes_cap: 8_192,
                    degraded: false,
                    attested_at_micros: 6_000_000,
                }),
                stopped: false,
            },
            HeldCustodyView {
                grant_id: vec![0x44; 16],
                owner: [0xA1; 32],
                scopes: None,
                retained_bytes_cap: 4_096,
                receipt: None,
                stopped: true,
            },
        ],
        offers: vec![CustodyOfferView {
            grant_id: vec![0x55; 16],
            owner: [0xA2; 32],
            scopes: CustodyScopeSet::Account,
            offered_at_micros: 7,
            // Nest-holdable, so the walk covers the target select whenever
            // the fixture-driven state also carries a pin.
            nest_can_hold: true,
        }],
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_devices_machine::{DeviceFolderRole, DeviceSummary};

    /// tui's own window: between the machine's build and the
    /// account-store-ready edge — or for a whole session whose assembly failed
    /// — the slot is empty, and the door must refuse rather than leave the
    /// machine to delete the nest row alone.
    ///
    /// Takes the door from a REAL machine built through [`DevicesState::build`]
    /// — not a hand-built `fleet_removal_door` — so the pin reds if `build`
    /// ever stops wiring the door at all, not only if the shared adapter's own
    /// refusal regresses (`fleet_removal.rs`'s pins already cover that half).
    ///
    /// Mutation: drop the `machine.set_fleet_removal(...)` call in
    /// [`DevicesState::build`] → `fleet_removal()` answers `None` → this pin
    /// reds (`cargo test -p fauna-tui --bins`).
    #[tokio::test]
    async fn the_removal_door_refuses_until_the_account_runtime_lands() {
        let nest = NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        );
        let state = DevicesState::build(
            nest,
            "",
            &[],
            crate::settings::follows_door(Default::default()),
        );
        let door = state
            .machine
            .as_ref()
            .expect("DevicesState::build always constructs a machine")
            .fleet_removal()
            .expect("DevicesState::build must wire the fleet-removal door");
        let refusal = door
            .resolve_removal("aa", Some([0x11; 32]))
            .await
            .expect_err("no runtime yet, so nothing may be removed");
        assert!(matches!(
            refusal,
            fauna_devices_machine::FleetRemovalRefusal::Unavailable(_)
        ));
    }

    fn device(id: &str, label: &str, online: bool, guardian_marked: bool) -> DeviceSummary {
        DeviceSummary {
            device_id: id.to_string(),
            label: label.to_string(),
            capabilities: "read,write".to_string(),
            registered_at: 0,
            last_seen_at: 0,
            online,
            guardian_marked,
            principal: None,
            folders: Vec::<DeviceFolderRole>::new(),
            p2p_participation: None,
            p2p_off_requested: false,
            p2p_participation_paint: None,
        }
    }

    fn snapshot(devices: Vec<DeviceSummary>) -> DevicesSnapshot {
        DevicesSnapshot {
            devices,
            ..Default::default()
        }
    }

    /// A member card as the shared machine renders it — the fingerprint
    /// through the one formatter, the way `DevicesMachine::refresh` fills it.
    fn member(id: u8, enrolled_at_ms: i64) -> fauna_devices_machine::FleetMemberSummary {
        let raw = [id; 32];
        fauna_devices_machine::FleetMemberSummary {
            device_id: fauna_core::hex32::encode(&raw),
            fingerprint: fauna_core::format::fleet_fingerprint(&raw),
            enrolled_at_ms,
        }
    }

    fn ids_of(els: &[Element]) -> Vec<&str> {
        els.iter().map(|e| e.id.as_str()).collect()
    }

    // ── Signed-in devices without a matching entry ───────────────────────────

    /// A settled honest fleet lists nobody: with no members the group paints
    /// nothing — no title chrome, no note, no card — so the page is exactly
    /// the roster it always was.
    #[test]
    fn no_members_paints_no_group() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "laptop", true, false)])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(
            !ids_of(&els)
                .iter()
                .any(|id| id.starts_with("device-member-")),
            "{:?}",
            ids_of(&els)
        );
        assert!(!els.iter().any(|e| e.text == t::MEMBERS_TITLE));
    }

    /// **A member no roster row accounts for paints its card by key, with the
    /// two-step remove** (`ui/devices.md` § Members without a matching
    /// entry): the group's title and note come first, then one card carrying
    /// the fingerprint the shared machine rendered, the claimed sign-in time
    /// as a local timestamp, and the single remove button — the confirm and
    /// cancel pair only once THAT card is armed, and only on that card.
    #[test]
    fn a_member_without_a_matching_entry_paints_its_card_with_a_two_step_remove() {
        let mut snap = snapshot(vec![device("d1", "laptop", true, false)]);
        snap.members = vec![member(0x22, 1_700_000_000_000), member(0x33, 0)];
        let state = DevicesState {
            snapshot: Some(snap),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let ids = ids_of(&els);
        let title = els
            .iter()
            .position(|e| e.text == t::MEMBERS_TITLE)
            .expect("the group title paints as chrome");
        assert!(els[title].id.is_empty(), "the title carries no invented id");
        assert_eq!(
            &ids[title + 1..title + 5],
            [
                "device-member-note",
                "device-member-card",
                "device-member-fingerprint",
                "device-member-enrolled-at",
            ]
        );
        assert_eq!(els[title + 1].text, t::MEMBER_NOTE);
        let fp = &els[title + 3];
        assert_eq!(
            fp.text,
            format!(
                "Device {}",
                fauna_core::format::fleet_fingerprint(&[0x22; 32])
            )
        );
        assert_eq!(fp.path, vec![("device-member-card".to_string(), 0)]);
        let when = &els[title + 4];
        assert!(when.text.starts_with("Says it signed in "), "{}", when.text);
        assert!(
            !when.text.contains("{when}") && when.text.contains("20"),
            "the instant is formatted locally, never left as a placeholder: {}",
            when.text
        );
        let remove: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "device-member-remove-button")
            .collect();
        assert_eq!(remove.len(), 2, "one remove per card, none armed");
        assert!(matches!(
            remove[1].gesture(),
            Some(Gesture::Settings(Action::RemoveMember(k)))
                if *k == fauna_core::hex32::encode(&[0x33; 32])
        ));
        assert!(!ids.contains(&"device-member-remove-confirm-button"));

        // Arm the SECOND card: its remove gives way to confirm + cancel, the
        // first card keeps its remove.
        let state = DevicesState {
            member_remove_pending: Some(fauna_core::hex32::encode(&[0x33; 32])),
            ..state
        };
        let els = devices_elements(&state);
        let by_id = |id: &str| -> Vec<&Element> { els.iter().filter(|e| e.id == id).collect() };
        assert_eq!(by_id("device-member-remove-button").len(), 1);
        assert_eq!(
            by_id("device-member-remove-button")[0].path,
            vec![("device-member-card".to_string(), 0)]
        );
        let confirm = by_id("device-member-remove-confirm-button");
        let cancel = by_id("device-member-remove-cancel-button");
        assert_eq!((confirm.len(), cancel.len()), (1, 1));
        assert_eq!(confirm[0].path, vec![("device-member-card".to_string(), 1)]);
        assert_eq!(confirm[0].text, t::MEMBER_REMOVE_CONFIRM);
        assert!(matches!(
            confirm[0].gesture(),
            Some(Gesture::Settings(Action::ConfirmRemoveMember(k)))
                if *k == fauna_core::hex32::encode(&[0x33; 32])
        ));
        assert!(matches!(
            cancel[0].gesture(),
            Some(Gesture::Settings(Action::CancelRemoveMember))
        ));
    }

    /// **An armed card stays armed on its own id across a snapshot that
    /// inserts a card before it** (the probe of the verify-back that minted
    /// the key form): armed at `[thief]`, a refresh lists `[sibling, thief]`;
    /// the confirm paints on the thief's card — now position 1 — and no
    /// other, and dispatches the thief's id, never the sibling's.
    #[test]
    fn an_armed_member_card_stays_armed_on_its_id_across_a_reshaping_snapshot() {
        let thief = fauna_core::hex32::encode(&[0x33; 32]);
        let mut snap = snapshot(vec![device("d1", "laptop", true, false)]);
        snap.members = vec![member(0x22, 0), member(0x33, 0)];
        let state = DevicesState {
            snapshot: Some(snap),
            member_remove_pending: Some(thief.clone()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let confirm: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "device-member-remove-confirm-button")
            .collect();
        assert_eq!(confirm.len(), 1);
        assert_eq!(confirm[0].path, vec![("device-member-card".to_string(), 1)]);
        assert!(matches!(
            confirm[0].gesture(),
            Some(Gesture::Settings(Action::ConfirmRemoveMember(k))) if *k == thief
        ));
    }

    /// **This device's own fingerprint paints on its own row, beside the
    /// this-device marker, and reads the same string a member card would
    /// read for that id** — the user compares like with like. Absent (no
    /// element) until the runtime answered, and never on another row.
    #[test]
    fn the_own_row_paints_this_devices_fingerprint_with_the_member_formatter() {
        let me = [0x0a; 32];
        let mut snap = snapshot(vec![
            device("d0", "kids-phone", true, false),
            device("d1", "this-fauna-tui", true, false),
        ]);
        snap.own_fleet_id = Some(fauna_core::hex32::encode(&me));
        snap.own_fingerprint = Some(fauna_core::format::fleet_fingerprint(&me));
        // The honest re-minted machine: a stale principal of this same
        // device lists as a member — its card must read the SAME fingerprint
        // shape the own row does, or elimination fires on the wrong card.
        snap.members = vec![member(0x0a, 7)];
        let state = DevicesState {
            snapshot: Some(snap),
            local_device_id: Some("d1".to_string()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let own: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "device-own-fingerprint")
            .collect();
        assert_eq!(own.len(), 1, "exactly the marked row carries it");
        assert_eq!(own[0].path, vec![("device-card".to_string(), 1)]);
        let fp = fauna_core::format::fleet_fingerprint(&me);
        assert_eq!(own[0].text, format!("Key {fp}"));
        let card_fp = els
            .iter()
            .find(|e| e.id == "device-member-fingerprint")
            .expect("the member card");
        assert_eq!(card_fp.text, format!("Device {fp}"));

        // No answer from the runtime yet: no element, even on the marked row.
        let mut state = state;
        state.snapshot.as_mut().unwrap().own_fingerprint = None;
        assert!(
            !devices_elements(&state)
                .iter()
                .any(|e| e.id == "device-own-fingerprint")
        );
    }

    #[test]
    fn empty_roster_paints_only_the_heading_the_copy_btn_and_nav_back() {
        let state = DevicesState::default();
        let els = devices_elements(&state);
        // `peer-actor-id-copy-btn` is page-level and renders even with an
        // empty roster — pairing the FIRST device is exactly when it's needed.
        assert_eq!(els.len(), 3);
        assert_eq!(els[0].id, "page-heading");
        assert_eq!(els[1].id, "peer-actor-id-copy-btn");
        assert_eq!(els[2].id, "settings-nav-back");
    }

    #[test]
    fn one_device_paints_a_full_row() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "my-laptop", true, false)])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let ids: Vec<&str> = els.iter().map(|e| e.id.as_str()).collect();
        assert_eq!(
            ids,
            [
                "page-heading",
                "peer-actor-id-copy-btn",
                "device-card",
                "device-name",
                "device-status",
                "device-p2p-participation-toggle",
                "device-remove-button",
                "settings-nav-back",
            ]
        );
        assert_eq!(els[3].text, "my-laptop");
        assert_eq!(els[4].text, "Online");
    }

    /// The keyless-posture badge (`devices.md` § Custody facet piece 1):
    /// renders ONLY where the derived fact is positive — a known principal
    /// with no wrap at the resolved tip. The three fail-safes each render
    /// nothing: no resolved tip (`keyed_principals` None), a row
    /// with no principal, and a keyed principal.
    #[test]
    fn keyless_posture_badge_renders_only_on_derived_keyless_rows() {
        let keyed_p = [0xA1u8; 32];
        let keyless_p = [0xB2u8; 32];
        let mut keyed_dev = device("aa", "Keyed", true, false);
        keyed_dev.principal = Some(fauna_core::hex32::encode(&keyed_p));
        let mut keyless_dev = device("bb", "Kiosk", true, false);
        keyless_dev.principal = Some(fauna_core::hex32::encode(&keyless_p));
        let unenrolled_dev = device("cc", "Unenrolled", true, false); // principal None

        let rows = vec![keyed_dev, keyless_dev, unenrolled_dev];
        let state = DevicesState {
            snapshot: Some(snapshot(rows.clone())),
            keyed_principals: Some([keyed_p].into_iter().collect()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let badges: Vec<_> = els
            .iter()
            .filter(|e| e.id == "device-keyless-posture-badge")
            .collect();
        assert_eq!(badges.len(), 1, "exactly the derived-keyless row");
        assert_eq!(badges[0].text, t::KEYLESS_POSTURE_BADGE);
        assert!(
            badges[0]
                .path
                .iter()
                .any(|s| s.0 == "device-card" && s.1 == 1),
            "scoped under the KIOSK row (index 1): {:?}",
            badges[0].path
        );

        // No resolved tip → NO badge anywhere, never a guessed posture.
        let state = DevicesState {
            snapshot: Some(snapshot(rows)),
            keyed_principals: None,
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(
            !els.iter().any(|e| e.id == "device-keyless-posture-badge"),
            "an unresolved world renders nothing"
        );
    }

    #[test]
    fn offline_device_paints_offline_status() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "my-phone", false, false)])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let status = els.iter().find(|e| e.id == "device-status").unwrap();
        assert_eq!(status.text, "Offline");
    }

    #[test]
    fn guardian_marked_device_paints_the_badge() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "wards-tablet", true, true)])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(els.iter().any(|e| e.id == "device-guardian-mark-badge"));
    }

    /// The marker is scoped to ITS card: with a marked device beside two
    /// unmarked ones, the badge's ancestor path is exactly that row's
    /// `device-card[i]`, and every other row child is scoped to its own row too.
    /// This is what lets the ward see WHICH device the guardian enrolled — a
    /// flat badge answers "is anything marked", never "is THIS one marked"
    /// (`family-safety.md` § Full visibility for young children; the Slice-F
    /// e2e reads it as `scope="device-card[i]"`).
    #[test]
    fn the_guardian_badge_is_scoped_to_its_own_card() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![
                device("d0", "kids-phone", true, false),
                device("d1", "parents-tablet", true, true),
                device("d2", "fauna-tui", true, false),
            ])),
            ..Default::default()
        };
        let els = devices_elements(&state);

        let badges: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "device-guardian-mark-badge")
            .collect();
        assert_eq!(badges.len(), 1, "exactly the marked device carries a badge");
        assert_eq!(
            badges[0].path,
            vec![("device-card".to_string(), 1)],
            "the badge hangs under the MARKED row's card, not flat"
        );

        // The card anchors themselves stay unscoped (they are the containers),
        // and each row's children carry that row's index.
        for (i, id) in ["device-name", "device-status", "device-remove-button"]
            .into_iter()
            .flat_map(|id| (0..3).map(move |i| (i, id)))
        {
            let el = els
                .iter()
                .filter(|e| e.id == id)
                .nth(i)
                .unwrap_or_else(|| panic!("{id} row {i}"));
            assert_eq!(
                el.path,
                vec![("device-card".to_string(), i)],
                "{id} row {i} must be scoped under its own card"
            );
        }
        assert!(
            els.iter()
                .filter(|e| e.id == "device-card")
                .all(|e| e.path.is_empty()),
            "the card anchor is the container, never scoped under itself"
        );
    }

    #[test]
    fn a_device_with_no_folders_paints_no_role_chip() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "my-laptop", true, false)])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(!els.iter().any(|e| e.id == "device-folder-role-badge"));
    }

    fn place(name: &str, o: bool, a: bool, d: bool) -> DeviceFolderRole {
        DeviceFolderRole {
            name: name.to_string(),
            originates: o,
            accepts: a,
            applies_deletes: d,
        }
    }

    #[test]
    fn a_device_in_three_sets_paints_three_place_chips_scoped_to_its_card() {
        use fauna_i18n::strings::devices::wizard as w;
        let mut d = device("d1", "backup-box", true, false);
        d.folders = vec![
            place("photos", true, false, false),
            place("docs", true, true, true),
            place("archive", true, true, false),
        ];
        let state = DevicesState {
            snapshot: Some(snapshot(vec![d])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let chips: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "device-folder-role-badge")
            .collect();
        assert_eq!(chips.len(), 3);
        assert_eq!(chips[0].text, w::PLACE_ORIGINATES);
        assert_eq!(
            chips[1].text,
            format!(
                "{} · {} · {}",
                w::PLACE_ORIGINATES,
                w::PLACE_ACCEPTS,
                w::PLACE_APPLIES_DELETES
            )
        );
        assert_eq!(
            chips[2].text,
            format!("{} · {}", w::PLACE_ORIGINATES, w::PLACE_ACCEPTS)
        );
        // Every chip hangs under this device's OWN card — the same
        // scoping law the guardian badge test above pins.
        for chip in &chips {
            assert_eq!(chip.path, vec![("device-card".to_string(), 0)]);
        }
    }

    #[test]
    fn the_place_chip_resolves_its_nested_keys() {
        // The chip text is composed through `device_place_label` and resolved
        // NESTED — a plain resolve would paint the raw
        // `devices.wizard.place_*` keys inside the template.
        let mut d = device("d1", "phone", true, false);
        d.folders = vec![place("photos", false, false, false)];
        let state = DevicesState {
            snapshot: Some(snapshot(vec![d])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let chip = els
            .iter()
            .find(|e| e.id == "device-folder-role-badge")
            .unwrap();
        assert_eq!(chip.text, fauna_i18n::strings::devices::PLACE_NONE);
        assert!(!chip.text.contains("devices."));
    }

    #[test]
    fn unmarked_device_paints_no_badge() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "my-laptop", true, false)])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(!els.iter().any(|e| e.id == "device-guardian-mark-badge"));
    }

    fn toggle_of(els: &[Element], row: usize) -> &Element {
        els.iter()
            .find(|e| {
                e.id == "device-p2p-participation-toggle"
                    && e.path == vec![("device-card".to_string(), row)]
            })
            .expect("every device-card carries the participation toggle")
    }

    fn checked_of(el: &Element) -> bool {
        match &el.role {
            crate::element::Role::Checkbox { checked, .. } => *checked,
            other => panic!("the toggle is a checkbox, got {other:?}"),
        }
    }

    /// `device-p2p-participation-toggle` (`p2p.md` § Per-device
    /// participation): the app draws exactly the paint the shared machine
    /// published for the row — the rule itself (own row vs sibling, local
    /// switch vs report) is `fauna_devices_machine::p2p_participation`'s,
    /// tested there. The own row's off switch is enabled, labelled as the
    /// device's own, and its click turns it on.
    #[test]
    fn the_toggle_paints_what_the_machine_published() {
        let mut own = device("d1", "this-fauna-tui", true, false);
        // The nest still holds a stale `on` report: the machine painted the
        // device-local `off`, and that is what shows.
        own.p2p_participation = Some(true);
        own.p2p_participation_paint = Some(fauna_devices_machine::p2p_participation_paint(
            true,
            Some(false),
            Some(true),
            false,
        ));
        let mut asked = device("d0", "tablet", true, false);
        asked.p2p_participation = Some(true);
        asked.p2p_off_requested = true;
        asked.p2p_participation_paint = Some(fauna_devices_machine::p2p_participation_paint(
            false,
            Some(false),
            Some(true),
            true,
        ));
        let state = DevicesState {
            snapshot: Some(snapshot(vec![asked, own])),
            ..Default::default()
        };
        let els = devices_elements(&state);

        let own = toggle_of(&els, 1);
        assert!(!checked_of(own), "the published local `off` shows");
        assert!(own.enabled, "the own switch is actionable");
        assert_eq!(own.text, t::P2P_PARTICIPATION_OWN);
        match &own.role {
            crate::element::Role::Checkbox {
                gesture: Gesture::Settings(Action::SetP2pParticipation { index, on }),
                ..
            } => {
                assert_eq!(*index, 1);
                assert!(*on, "an off switch's gesture turns it on");
            }
            other => panic!("unexpected role {other:?}"),
        }

        let asked = toggle_of(&els, 0);
        assert!(checked_of(asked));
        assert_eq!(asked.text, t::P2P_PARTICIPATION_OFF_REQUESTED);
        assert!(!asked.enabled, "already asked: nothing more to send");
    }

    /// Own-ness is the machine's call, never the app's: a row matching the
    /// app's own device id but carrying no machine paint (no door answered)
    /// paints the sibling arm — its report, turn-off only — even though its
    /// `device-this-mark-badge` still shows.
    #[test]
    fn the_apps_own_id_alone_never_makes_the_own_switch() {
        let mut mine = device("d1", "this-fauna-tui", true, false);
        mine.p2p_participation = Some(false);
        let mut snap = snapshot(vec![mine]);
        snap.own_p2p_participation = Some(true);
        let state = DevicesState {
            snapshot: Some(snap),
            local_device_id: Some("d1".to_string()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(els.iter().any(|e| e.id == "device-this-mark-badge"));
        let toggle = toggle_of(&els, 0);
        assert_eq!(toggle.text, t::P2P_PARTICIPATION);
        assert!(!checked_of(toggle), "the row's report, not the local read");
        assert!(!toggle.enabled, "the sibling arm cannot turn it on");
    }

    /// `device-this-mark-badge` (`devices.md` § This-device marker) — a pure
    /// client-side match against the row the app ENROLLED on, no nest-side
    /// flag. With no enrolled row known this falls back to the app's own
    /// locally-stored id, which is what this case exercises.
    #[test]
    fn the_row_matching_local_device_id_paints_the_this_device_badge() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![
                device("d0", "kids-phone", true, false),
                device("d1", "this-fauna-tui", true, false),
            ])),
            local_device_id: Some("d1".to_string()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let badges: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "device-this-mark-badge")
            .collect();
        assert_eq!(
            badges.len(),
            1,
            "exactly the matching device carries a badge"
        );
        assert_eq!(
            badges[0].path,
            vec![("device-card".to_string(), 1)],
            "the badge hangs under the MATCHING row's card, not flat"
        );
    }

    /// **Row 176 — the defect this badge had until 2026-08-19.** A co-located
    /// sync agent provisioned by a *different* app on this box advertises its
    /// own id, so decision 2 converges this app's enrollment onto the agent's
    /// row (`sync-agent.md` § Credential model → the RULED 2026-08-15
    /// block). The app's own `device.db` id then names **no roster row at
    /// all** — `d-own` is deliberately absent from the snapshot here, exactly
    /// as it is absent from the nest's roster. Marking it marked nothing, and
    /// on a family-safety surface that is a guardian who cannot find their own
    /// device.
    #[test]
    fn the_enrolled_row_is_marked_when_it_differs_from_the_apps_own_id() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![
                device("d0", "kids-phone", true, false),
                device("d-agent", "this-box", true, false),
            ])),
            local_device_id: Some("d-own".to_string()),
            enrolled_device_row: Some("d-agent".to_string()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let badges: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "device-this-mark-badge")
            .collect();
        assert_eq!(
            badges.len(),
            1,
            "the enrolled row carries the badge — and the app's own id, which \
             names no row, carries nothing"
        );
        assert_eq!(
            badges[0].path,
            vec![("device-card".to_string(), 1)],
            "on the AGENT's row (index 1), not the app's own"
        );
    }

    /// The other side of the same rule: an enrolled row that *is* the app's
    /// own id (decision 2 cases 1 and 3 — no agent, or one this gate refuses)
    /// still marks that row. A fix that unconditionally preferred a probe
    /// result would blank the badge here, which is why the fallback is a rule
    /// and not a formality.
    #[test]
    fn the_enrolled_row_equal_to_the_own_id_still_marks_that_row() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "my-laptop", true, false)])),
            local_device_id: Some("d1".to_string()),
            enrolled_device_row: Some("d1".to_string()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert_eq!(
            els.iter()
                .filter(|e| e.id == "device-this-mark-badge")
                .count(),
            1
        );
    }

    #[test]
    fn no_local_device_id_paints_no_this_device_badge() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device("d1", "my-laptop", true, false)])),
            local_device_id: None,
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(!els.iter().any(|e| e.id == "device-this-mark-badge"));
    }

    /// The two badges are independent: a guardian marking their own enrolled
    /// device legitimately carries both.
    #[test]
    fn a_device_can_carry_both_badges() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![device(
                "d1",
                "guardians-own-device",
                true,
                true,
            )])),
            local_device_id: Some("d1".to_string()),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert!(els.iter().any(|e| e.id == "device-guardian-mark-badge"));
        assert!(els.iter().any(|e| e.id == "device-this-mark-badge"));
    }

    #[test]
    fn two_devices_each_get_their_own_row_and_index() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![
                device("d1", "first", true, false),
                device("d2", "second", false, false),
            ])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        assert_eq!(els.iter().filter(|e| e.id == "device-card").count(), 2);
        let names: Vec<&str> = els
            .iter()
            .filter(|e| e.id == "device-name")
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(names, ["first", "second"]);
        // The remove-button gestures carry the array index, not the device id —
        // `DevicesMachine::remove_device` resolves by position.
        let remove_indices: Vec<u32> = els
            .iter()
            .filter(|e| e.id == "device-remove-button")
            .map(|e| match &e.role {
                crate::element::Role::Button(Gesture::Settings(Action::RemoveDevice(i))) => *i,
                _ => panic!("expected a RemoveDevice gesture"),
            })
            .collect();
        assert_eq!(remove_indices, [0, 1]);
    }

    /// `peer-actor-id-copy-btn` is now page-level: it fires `CopyActorId` (which
    /// reads the client's OWN actor id at dispatch time, not any device's id),
    /// renders exactly once regardless of roster size, and is never scoped
    /// under a `device-card`.
    #[test]
    fn actor_id_copy_button_is_single_instance_and_copies_the_clients_own_actor_id() {
        let state = DevicesState {
            snapshot: Some(snapshot(vec![
                device("d0", "first", true, false),
                device("d1", "second", true, false),
            ])),
            ..Default::default()
        };
        let els = devices_elements(&state);
        let buttons: Vec<&Element> = els
            .iter()
            .filter(|e| e.id == "peer-actor-id-copy-btn")
            .collect();
        assert_eq!(
            buttons.len(),
            1,
            "exactly one instance, regardless of roster size"
        );
        assert!(
            buttons[0].path.is_empty(),
            "unscoped — never hangs under a device-card"
        );
        match &buttons[0].role {
            crate::element::Role::Button(Gesture::Settings(Action::CopyActorId)) => {}
            _ => panic!("expected a page-level CopyActorId gesture"),
        }
    }
}

#[cfg(test)]
mod succession_custody_tests {
    use super::*;

    /// **The call-site pin for the label plane's read fallback.**
    ///
    /// Same reason as `sync_agent`'s twin: `LabelCustody`'s own tests cannot see
    /// *this* function stop passing the retired roots — the lesson, and
    /// the shape caught. `predecessor_count()` is the accessor that
    /// exists for exactly this assertion.
    ///
    /// Mutation: drop the `.with_predecessors(..)` call → this reds.
    #[test]
    fn the_devices_custody_offers_the_accounts_retired_roots() {
        let retired = vec![fauna_core::crypto::BackupKey::from_bytes([0x11u8; 32])];
        let custody = label_custody(
            nest_for_test(),
            [7u8; 32],
            Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
            &retired,
        );

        assert_eq!(
            custody.predecessor_count(),
            1,
            "after a succession this account's device labels and owner-audience \
             include/exclude path lists are still sealed under a predecessor's \
             root, and the degrade is SILENT (Omit / no-list)"
        );
        // Positive controls on the same assembly, so a red above is about the
        // predecessor threading and not about custody construction in general.
        assert!(
            custody.has_resolver(),
            "the shared-set resolver must still be wired"
        );
        assert!(
            custody.has_owner_key(),
            "the current owner key must still be wired"
        );
    }

    /// The negative control — an identity that never succeeded offers none.
    #[test]
    fn the_devices_custody_offers_nothing_for_a_never_succeeded_identity() {
        let custody = label_custody(
            nest_for_test(),
            [7u8; 32],
            Arc::new(fauna_client_folders::MemoryFolderKeyStore::default()),
            &[],
        );
        assert_eq!(custody.predecessor_count(), 0);
    }

    fn nest_for_test() -> Arc<NestClient> {
        NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        )
    }

    // ── The T16 custody facet render ─────────────────────────────────────────

    fn custody_facet() -> CustodyFacetSnapshot {
        super::custody_walk_facet()
    }

    fn custody_state() -> DevicesState {
        let facet = custody_facet();
        DevicesState {
            custody_budget_drafts: seed_budget_drafts(&facet),
            custody: Some(facet),
            // No roster snapshot on purpose: the render treats `None` as an
            // empty roster, and the families under test are the custody ones.
            ..Default::default()
        }
    }

    /// The three families render with their approved IDs, each child scoped
    /// under its own card — and the state distinctions the spec demands are
    /// visible: three receipt states are three different strings, a pending
    /// ceremony cannot revoke, a stopped hold cannot stop again, and the
    /// degraded marker rides the bytes line.
    #[test]
    fn custody_families_render_scoped_with_honest_states() {
        let els = devices_elements(&custody_state());
        let by_id = |id: &str| -> Vec<&Element> { els.iter().filter(|e| e.id == id).collect() };

        // Owner side: two cards — the fixture's THIRD row is nest-anchored
        // (the nest-custodian identity fact) and must stay off this page
        // entirely; it renders on the Nests page's nest-trust-custody-*
        // family instead (one custody never renders in both).
        assert_eq!(by_id("custody-holder-card").len(), 2);
        assert!(
            els.iter().all(|e| !e.id.starts_with("nest-trust-custody")),
            "the nest-anchored custody must not render on Devices"
        );
        let revokes = by_id("custody-holder-revoke-button");
        assert_eq!(revokes.len(), 2);
        assert!(revokes[0].enabled && !revokes[1].enabled);
        // Fresh vs no-receipt-yet are DIFFERENT strings (the A7 honesty rule).
        let statuses = by_id("custody-holder-receipt-status");
        assert_ne!(statuses[0].text, statuses[1].text);
        assert_eq!(statuses[1].text, t::CUSTODY_RECEIPT_NONE);
        // The degraded marker rides the bytes line of the degraded receipt.
        let bytes = by_id("custody-holder-held-bytes");
        assert!(bytes[0].text.contains(t::CUSTODY_DEGRADED_BADGE));
        // The REQUIRED honest-bound copy renders on the live card's own line.
        let cards = by_id("custody-holder-card");
        assert_eq!(cards[0].text, t::CUSTODY_REVOKE_BOUND_NOTE);
        // Children scope under their card.
        assert!(
            statuses[1]
                .path
                .iter()
                .any(|s| s.0 == "custody-holder-card" && s.1 == 1),
            "row children register .within(custody-holder-card, i)"
        );

        // Host side: the stopped hold's stop button is disabled; the budget
        // input carries the seeded draft (the budget in force).
        let stops = by_id("custody-held-stop-button");
        assert_eq!(stops.len(), 2);
        assert!(stops[0].enabled && !stops[1].enabled);
        let budgets = by_id("custody-held-budget-input");
        assert_eq!(budgets[0].text, crate::format::byte_size(8_192));
        // A receipt-less hold renders the bytes line with placeholders,
        // never empty.
        let held_bytes = by_id("custody-held-bytes");
        assert!(!held_bytes[1].text.is_empty());

        // The consent card: the REQUIRED floor copy, and both verdict
        // buttons live.
        assert_eq!(by_id("custody-offer-card").len(), 1);
        assert_eq!(
            by_id("custody-offer-floor-note")[0].text,
            t::CUSTODY_OFFER_FLOOR
        );
        assert!(by_id("custody-offer-accept-button")[0].enabled);
        assert!(by_id("custody-offer-decline-button")[0].enabled);
        // No pinned nest identity in this state → the target select is
        // ABSENT (never disabled) even for an offer a nest can hold.
        assert!(
            by_id("custody-offer-target-select").is_empty(),
            "no pin → no nest choice"
        );
    }

    /// The host-side choice (the nest-custodian identity fact): the target
    /// select renders only with BOTH an offer a nest can hold and a pinned nest
    /// identity, defaults to the device, and reflects the per-grant pick.
    #[test]
    fn custody_offer_target_select_needs_a_nest_holdable_offer_and_pin() {
        let mut state = custody_state();
        state.custody_nest_pin = Some([0xAB; 32]);
        let els = devices_elements(&state);
        let selects: Vec<_> = els
            .iter()
            .filter(|e| e.id == "custody-offer-target-select")
            .collect();
        assert_eq!(selects.len(), 1, "pin + nest-holdable offer → the choice");
        assert_eq!(
            selects[0].text,
            t::CUSTODY_OFFER_TARGET_DEVICE,
            "the device is the default"
        );
        assert!(
            selects[0]
                .path
                .iter()
                .any(|s| s.0 == "custody-offer-card" && s.1 == 0),
            "the select scopes under its card"
        );

        // The pick flips the painted selection (keyed by grant id).
        state.custody_offer_nest_choice.insert(vec![0x55; 16]);
        let els = devices_elements(&state);
        let select = els
            .iter()
            .find(|e| e.id == "custody-offer-target-select")
            .expect("still offered");
        assert_eq!(select.text, t::CUSTODY_OFFER_TARGET_NEST);

        // An offer naming no owner nest never shows the choice, pin or no pin.
        let mut facet = custody_facet();
        facet.offers[0].nest_can_hold = false;
        state.custody = Some(facet);
        let els = devices_elements(&state);
        assert!(
            !els.iter().any(|e| e.id == "custody-offer-target-select"),
            "no owner nest → no nest choice"
        );
    }

    /// The offer-initiation flow: closed paints only the button; open paints
    /// the host select + the REQUIRED floor copy + confirm/cancel, with the
    /// confirm gated on a chosen host.
    #[test]
    fn custody_mint_flow_paints_gated_on_a_chosen_host() {
        let mut state = custody_state();
        let els = devices_elements(&state);
        assert_eq!(
            els.iter().filter(|e| e.id == "custody-mint-button").count(),
            1
        );
        assert!(!els.iter().any(|e| e.id == "custody-mint-host-select"));

        state.custody_mint_open = true;
        state.custody_mint_candidates = vec![
            fauna_client_conversations::CustodyMintCandidate {
                host: fauna_core::identity::ActorId([0xB0; 32]),
                channel_hex: "aa".repeat(32),
                label: "friend".to_string(),
            },
            fauna_client_conversations::CustodyMintCandidate {
                host: fauna_core::identity::ActorId([0xB1; 32]),
                channel_hex: "bb".repeat(32),
                label: "other".to_string(),
            },
        ];
        let els = devices_elements(&state);
        let select = els
            .iter()
            .find(|e| e.id == "custody-mint-host-select")
            .expect("host select paints while open");
        match &select.role {
            crate::element::Role::Select { options, .. } => {
                assert_eq!(options, &vec!["friend".to_string(), "other".to_string()]);
            }
            other => panic!("expected a select, got {other:?}"),
        }
        assert_eq!(select.text, t::CUSTODY_MINT_HOST_PLACEHOLDER);
        assert_eq!(
            els.iter()
                .find(|e| e.id == "custody-mint-floor-note")
                .expect("the floor copy is REQUIRED before confirm")
                .text,
            t::CUSTODY_MINT_FLOOR
        );
        assert!(
            !els.iter()
                .find(|e| e.id == "custody-mint-confirm-button")
                .unwrap()
                .enabled,
            "no host chosen → confirm disabled"
        );

        state.custody_mint_selected = Some(1);
        let els = devices_elements(&state);
        assert!(
            els.iter()
                .find(|e| e.id == "custody-mint-confirm-button")
                .unwrap()
                .enabled
        );
        assert_eq!(
            els.iter()
                .find(|e| e.id == "custody-mint-host-select")
                .unwrap()
                .text,
            "other"
        );
    }
}
