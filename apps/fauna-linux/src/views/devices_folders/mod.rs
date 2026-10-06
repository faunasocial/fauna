//! Settings → **Devices** (the device roster) + Settings → **Folders** (the
//! folder control plane) — two settings sub-pages backed by **one** shared
//! `DevicesMachine` and **one** observer-driven render loop.
//!
//! The 2026-06-28 sync/folder UI unification (design tracked internally) removed
//! the top-level **Peers** page and split its surfaces in two:
//!
//! - **Devices** (`roster.rs`): the device roster only (`device-*`).
//! - **Folders** (`folders.rs` + `conflicts.rs` + `location_binding.rs` +
//!   `wizard.rs`): the one control plane for folders — list, create wizard,
//!   per-set config (selective-sync paths + conflict policy), local-folder binding
//!   (desktop, nested per set), and conflict resolution.
//!
//! Both pages render off the same `DevicesMachine::snapshot()` (no client-side
//! HTTP); a single render loop rewrites both pages' widgets on every observer
//! tick. Page gestures (remove device, delete folder, save paths, set
//! conflict policy, resolve conflict, bind/unbind a folder) forward through the machine
//! / the device-local folder map; the machine refreshes on auth + when either
//! sub-page becomes visible (wired in `app.rs`).

use fauna_ui_ids as ids;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;

use fauna_devices_machine::{DevicesMachine, DevicesObserver, MlsQuery, build_devices_machine};
use fauna_folders_machine::FolderWizardStep;

use crate::async_helper;
use crate::client::FaunaClient;
use crate::i18n::strings;
// `folders` here is the i18n string group; the sibling `pub mod folders`
// below is this page's own module, so the group takes an alias.
use crate::i18n::strings::{devices, folders as folder_strings};
use crate::sync::LocationBinding;
use crate::testid::set_test_id;

pub mod conflicts;
pub mod custody;
pub mod folders;
pub mod location_binding;
pub mod roster;
pub mod wizard;

#[cfg(any(debug_assertions, feature = "e2e-agent"))]
pub use location_binding::inject_locations_for_test;
pub use location_binding::rerender_bindings;

/// The repaint hook for the co-present ceremony's shared-set rows
/// ([`DevicesFoldersHandles::repaint_group_scopes`]).
#[cfg(feature = "p2p-share")]
pub type RepaintGroupScopes = Rc<dyn Fn(&[crate::offline_share::GroupScopeView])>;

/// One of the Devices sub-page's page-level reads, run on every nav edge.
type NavHook = Rc<dyn Fn()>;

/// Handles the two sub-pages hand back to `app.rs`.
pub struct DevicesFoldersHandles {
    /// The Devices sub-page's page-level reads (custody facet + the account
    /// store's enrolled row, refusal and keyed set), for the settings shell's
    /// re-select edge — the page's map hook covers arriving from elsewhere.
    pub devices_nav_refresh: Rc<dyn Fn()>,
    /// Folder list box — kept so app.rs's `FolderMembersLoaded` dispatch can
    /// populate the lazy-loaded member roster into the right expander row (the
    /// member roster is a row-detail WS-RPC read the `DevicesMachine` doesn't own).
    pub folder_list_box: gtk::ListBox,
    /// Recipient-side "Shared with you" pending-share section (group + inner list),
    /// so app.rs's `FolderPendingSharesLoaded` dispatch can rebuild the staged
    /// cross-user shares + toggle the section's visibility. A page-level read the
    /// `DevicesMachine` doesn't own (mirrors the owner-side actor roster).
    pub pending_shares_group: adw::PreferencesGroup,
    pub pending_shares_box: gtk::ListBox,
    /// The co-present offline-share ceremony's panel widgets, so app.rs's
    /// `DataMessage` handlers can repaint them on `OfflineShareSeatBound` /
    /// `OfflineShareProgressed` / `OfflineShareFailed` (`p2p.md` § Offline
    /// share initiation, row 334).
    #[cfg(feature = "p2p-share")]
    pub offline_share: folders::OfflineShareHandles,
    /// The peer-transfer surface's widgets, so app.rs's `SharePlaneChanged`
    /// handler can repaint the six ids from the share plane's state cell
    /// (`p2p.md` § Cross-user shared-set transfer, row 338).
    #[cfg(feature = "p2p-share")]
    pub share_transfer: folders::ShareTransferHandles,
    /// The ceremony's own state — shared with `wire_offline_share_section`'s
    /// click wiring (mod.rs-local) AND app.rs's `DataMessage` handlers, which
    /// is exactly why it is a cell rather than owned by either side alone.
    #[cfg(feature = "p2p-share")]
    pub offline_share_state: Rc<RefCell<crate::offline_share::OfflineShareState>>,
    /// Repaint the co-present ceremony's shared-set `folder-row`s from a fresh
    /// group listing: app.rs's `GroupSharesLoaded` arm hands it the scopes half
    /// of the same read that paints the consent cards.
    #[cfg(feature = "p2p-share")]
    pub repaint_group_scopes: RepaintGroupScopes,
    /// The shared-Rust `DevicesMachine`. app.rs refreshes it on auth + when either
    /// sub-page becomes visible (replacing the old per-nav HTTP fetch).
    pub devices_machine: Arc<DevicesMachine>,
}

/// Bridges `DevicesMachine` notifications to the GTK main loop: each `on_changed`
/// pushes a tick onto an `async-channel` the render loop drains. Mirrors the
/// wizard / onboarding `GtkObserver` pattern. `on_changed` may fire from a worker
/// thread (the async gestures run on a `run_on_tokio` runtime), so the
/// thread-safe `async_channel::Sender` is the hand-off.
struct GtkDevicesObserver {
    tx: async_channel::Sender<()>,
}

impl DevicesObserver for GtkDevicesObserver {
    fn on_changed(&self) {
        let _ = self.tx.try_send(());
    }
}

/// The linux impl of the B3 member-row join-filter seam ([`MlsQuery`]) — the
/// per-app glue the centralized `DevicesMachine` filter delegates to. `DevicesMachine::refresh` drops
/// every `role == "member"` row this returns `false` for, so a rostered-but-un-joined
/// knock never reaches the snapshot the page renders. Until this is wired
/// (`set_mls_query`), the machine is fail-safe (drops all member rows).
///
/// Reads the ONE live per-actor `MlsEngine` off the conversations rail
/// (`ConversationsSession::engine`, over the single `mls_state.db`) — never a second
/// engine racing the SQLite file. Derives the set's `ChannelId` the SAME way the nest
/// does (`ChannelId::from_group_id` over a plain-`hex` decode — openMLS group ids are
/// variable length, so NOT `hex32`). Pre-login (no session) → `false` (withheld); the
/// list re-filters on the next `refresh()` once the session is live.
struct LinuxMlsQuery;

impl MlsQuery for LinuxMlsQuery {
    fn is_joined_shared_set(&self, mls_group_id_hex: &str) -> bool {
        let Some(session) = crate::conversations::conv_backend::active_session() else {
            return false;
        };
        let Ok(channel_id) = crate::client::channel_id_from_group_id_hex(mls_group_id_hex) else {
            return false;
        };
        session.engine().has_group(&channel_id)
    }
}

/// The `page-heading` title label (ui.yaml global rule). `set_test_id` so AT-SPI
/// Description-based lookups resolve it.
fn page_heading(text: &str) -> gtk::Label {
    let heading = gtk::Label::builder()
        .label(text)
        .halign(gtk::Align::Start)
        .css_classes(["title-1"])
        .build();
    set_test_id(&heading, ids::PAGE_HEADING);
    heading
}

/// The page-level `error-message` label (e2e Rule 2), hidden until set.
fn error_label() -> gtk::Label {
    let label = gtk::Label::builder().visible(false).build();
    set_test_id(&label, ids::ERROR_MESSAGE);
    label
}

/// The **Devices** sub-page static shell — the device roster only.
struct DevicesShell {
    content: gtk::Box,
    device_list_box: gtk::ListBox,
    /// The T16 custody facet (`ui/devices.md` § Custody facet pieces 2 + 3 and
    /// offer initiation) — every family below the roster, each group hidden
    /// until the facet hydrate finds something for it.
    custody: Rc<custody::CustodyView>,
    error_label: gtk::Label,
}

fn build_devices_shell(actor_id: Option<&str>) -> DevicesShell {
    let identity_group = roster::build_identity_section(actor_id);
    let (devices_group, device_list_box) = roster::build_devices_section();
    let custody = custody::CustodyView::build();
    let error_label = error_label();
    let content = crate::views::layout::page_box(24);
    content.append(&page_heading(devices::TITLE)); // "Devices"
    content.append(&error_label);
    content.append(&identity_group);
    content.append(&devices_group);
    // The custody families sit BELOW the roster: they are not this account's
    // devices, and the facet is explicit that the groups never intermix
    // (`ui/devices.md` § Custody facet — "a distinctly-labeled group").
    custody.append_to(&content);
    DevicesShell {
        content,
        device_list_box,
        custody,
        error_label,
    }
}

/// The **Folders** sub-page static shell — the folder control plane (list +
/// wizard + per-set config + conflicts; the nested folder binding is built
/// per-row inside `folders::build_folder_row`).
struct FoldersShell {
    content: gtk::Box,
    folder_list_box: gtk::ListBox,
    folder_add_button: gtk::Button,
    conflicts_group: adw::PreferencesGroup,
    conflict_list_box: gtk::ListBox,
    /// Recipient-side "Shared with you" pending-share section (a stranger's staged
    /// cross-user share, awaiting accept/decline). Populated off
    /// `FolderPendingSharesLoaded` — not the `DevicesMachine` snapshot.
    pending_shares_group: adw::PreferencesGroup,
    pending_shares_box: gtk::ListBox,
    /// The co-present offline-share ceremony's panel (`p2p.md` § Offline
    /// share initiation, row 334) — sits directly under the knock list, the
    /// two being the same story from opposite ends (a share that reached you
    /// through your nest, and one being handed to you in person).
    #[cfg(feature = "p2p-share")]
    offline_share: folders::OfflineShareHandles,
    /// The page-level peer-transfer surface (`p2p.md` § Cross-user shared-set
    /// transfer, row 338) — what this device serves to peers and what the
    /// pump moved. Sits directly under the ceremony panel, because the
    /// ceremony is how a set gets onto this plane in the first place.
    #[cfg(feature = "p2p-share")]
    share_transfer: folders::ShareTransferHandles,
    /// The page-level "Folders you follow" section — its rows repaint from
    /// `snapshot.followed` on every tick, its two buttons are wired once the
    /// `FaunaClient` is available.
    followed: folders::FollowedSection,
    error_label: gtk::Label,
}

/// `fauna_client` is `None` only in widget unit tests — see
/// [`folders::build_sync_defaults_section`], the one section that needs it.
fn build_folders_shell(fauna_client: Option<&Rc<FaunaClient>>) -> FoldersShell {
    let (folders_group, folder_list_box, folder_add_button) = folders::build_folders_section();
    let (conflicts_group, conflict_list_box) = conflicts::build_conflicts_section();
    let (pending_shares_group, pending_shares_box) = folders::build_pending_shares_section();
    #[cfg(feature = "p2p-share")]
    let offline_share = folders::build_offline_share_section();
    #[cfg(feature = "p2p-share")]
    let share_transfer = folders::build_share_transfer_section();
    let followed = folders::build_followed_section();
    let sync_defaults_group = folders::build_sync_defaults_section(fauna_client);
    let error_label = error_label();
    let content = crate::views::layout::page_box(24);
    content.append(&page_heading(folder_strings::TITLE)); // "Folders"
    content.append(&error_label);
    // Incoming shares that need a decision sit at the top, above conflicts + the set
    // list, so a knock is unmissable (hidden until one arrives).
    content.append(&pending_shares_group);
    // The co-present ceremony's panel sits directly under the knock list —
    // the two are the same story from opposite ends (`p2p.md` § Offline share
    // initiation, row 334; mirrors tui's `offline_share_elements` placement).
    #[cfg(feature = "p2p-share")]
    content.append(&offline_share.group);
    // And the transfer surface directly under that: what this device is
    // serving to peers and what the pump moved — the plane the ceremony above
    // initiates into (mirrors tui's `share_transfer_elements` placement).
    #[cfg(feature = "p2p-share")]
    content.append(&share_transfer.group);
    content.append(&conflicts_group);
    content.append(&folders_group);
    // "Folders you follow" sits below the user's OWN folders: these are somebody
    // else's, read-only, and the section is always offered (the follow button is
    // how a user gets their first one) — `ui/folders.md` § Following a public
    // folder, same placement tui uses.
    content.append(&followed.group);
    // Page-level Sync defaults (the global default conflict policy for new
    // sets) below the per-set list — the page's first non-per-set control.
    content.append(&sync_defaults_group);
    FoldersShell {
        content,
        folder_list_box,
        folder_add_button,
        conflicts_group,
        conflict_list_box,
        pending_shares_group,
        pending_shares_box,
        #[cfg(feature = "p2p-share")]
        offline_share,
        #[cfg(feature = "p2p-share")]
        share_transfer,
        followed,
        error_label,
    }
}

/// Widgets + state the render loop rewrites on every observer tick.
struct RenderCtx {
    device_list_box: gtk::ListBox,
    folder_list_box: gtk::ListBox,
    /// The "Folders you follow" rows — repainted from `snapshot.followed`, a
    /// list SEPARATE from `folder_list_box` (a followed folder is not a
    /// `folder-row`; `ui/folders.md` § Following a public folder).
    followed_list_box: gtk::ListBox,
    conflicts_group: adw::PreferencesGroup,
    conflict_list_box: gtk::ListBox,
    /// Both sub-pages carry an `error-message` label; the single `snapshot.error`
    /// is painted onto both so a gesture failure surfaces wherever the user is.
    devices_error_label: gtk::Label,
    folders_error_label: gtk::Label,
    /// Device-local folder↔folder map (the in-process engine's source of truth),
    /// read per-set when rendering each `folder-row`'s nested binding.
    location_map: Rc<RefCell<Vec<LocationBinding>>>,
    /// Whether this actor can serve a set over WebDAV at all — it needs the MSEK
    /// that serving seals the `WebdavKeysBlob` under, minted when mail is first
    /// enabled. Not a `DevicesMachine` snapshot field: the machine is deliberately
    /// keyless, so the capability is read once off the key-bearing `FaunaClient`
    /// (`can_serve_webdav` → the shared `owner_can_serve_webdav`) and cached here.
    /// It gates each owner row's `folder-webdav-toggle` — disabled + hinted
    /// for an MSEK-less actor rather than clicking into a `NoMsek` that has already
    /// committed the nest flag (`webdav-server.md` § Independent enablement pt 2).
    /// Starts `false` (fail-safe: a control that cannot succeed is not offered).
    can_serve_webdav: Cell<bool>,
    /// The creator's own subscription tier names — the option set each web-type
    /// row's `folder-paywall-tier-select` offers. Like `can_serve_webdav`, a
    /// page-level read the keyless `DevicesMachine` can't own: fetched off the
    /// key-bearing `FaunaClient` (`fetch_own_tier_names`) on page-map and cached
    /// here. Empty ⇒ the select renders disabled with a "create a tier first" hint
    /// (nothing to paywall to). `docs/goal/ui/folders.md` § Web paywall.
    own_tiers: RefCell<Vec<String>>,
    /// The co-present ceremony's own shared-set listing — sets this device
    /// holds the machinery for, appended as `folder-row`s after the M2 sets
    /// (`p2p.md` § Offline share initiation). Painted through
    /// [`DevicesFoldersHandles::repaint_group_scopes`] from the scopes half of
    /// the SAME read that carries the consent cards (`GroupSharesLoaded`), so
    /// the two halves of the group surface can never disagree about a scope,
    /// and the listing refreshes on every edge the knock lists do: the page
    /// mapping, re-selecting the open page, sign-in, and a ceremony landing a
    /// scope.
    #[cfg(feature = "p2p-share")]
    group_scopes: RefCell<Vec<crate::offline_share::GroupScopeView>>,
    /// Parent for the wizard `adw::Dialog` (must be in a window when presented) —
    /// the Folders sub-page content.
    wizard_parent: gtk::Widget,
    /// The live wizard dialog, if one is open.
    wizard_view: RefCell<Option<wizard::FolderWizardView>>,
    /// This app's own locally-stored device id (hex), read once at
    /// construction — the **fallback** half of the `device-this-mark-badge`
    /// row match (`devices.md` § This-device marker);
    /// [`Self::enrolled_device_row`] is preferred over it, and
    /// `fauna_devices_machine::this_device_row` owns the rule. `None` if the
    /// store hasn't minted an id yet, or is unreadable — with no enrolled row
    /// either, the badge then simply never matches, the safe default.
    local_device_id: Option<String>,
    /// The `sync_devices` row this app **actually enrolled on**
    /// (`AccountStoreHandle::enrolled_device_row`) — the preferred half of the
    /// badge match. It differs from [`Self::local_device_id`] exactly when a
    /// co-located sync agent provisioned by a *different* app on this box
    /// advertises its own id, which decision 2 makes the enrollment target
    /// (`sync-agent.md` § Credential model → the RULED 2026-08-15
    /// block): this app's own id then names no roster row at all. Like
    /// `can_serve_webdav`, a page-level read the keyless `DevicesMachine`
    /// can't own — re-read on every Devices nav rather than once at build, so
    /// an enrollment that converges after sign-in is picked up without a
    /// restart. `None` = nothing enrolled for this actor yet, so the fallback
    /// stands.
    enrolled_device_row: RefCell<Option<String>>,
    /// The nest's **standing refusal** of this machine's enrollment
    /// (`AccountStoreHandle::enrollment_refusal` — today the tier device cap,
    /// `devices.md` § Step 4), painted on the Devices sub-page's
    /// `error-message` while it stands (`ui/devices.md` § Errors & edge
    /// cases). Re-read on every Devices nav beside `enrolled_device_row`, off
    /// the shared credential slot, so a refusal the co-located sync agent's
    /// pump met reaches this page even though the app runs no pass itself.
    /// `None` = nothing refused, or healed since.
    enrollment_refusal: RefCell<Option<fauna_sync_engine::account_runtime::EnrollmentRefusal>>,
    /// The device principals keyed at the account store's resolved tip
    /// (`AccountStoreHandle::keyed_principals`) — the keyless-posture marker's
    /// derived fact (`ui/devices.md` § Custody facet piece 1), joined per row by
    /// the shared `fauna_devices_machine::keyless_posture`. Re-read on every
    /// Devices nav beside `enrolled_device_row`. `None` = no resolved tip (or no
    /// store yet), which marks no row.
    keyed_principals: RefCell<Option<std::collections::BTreeSet<[u8; 32]>>>,
    /// The last custody gesture's error (`run_custody_act`'s error string, an
    /// unparseable budget, no one to ask) — painted on the Devices sub-page's
    /// `error-message` beside the machine's own error, so a machine tick cannot
    /// wipe it (e2e convention 11: never a silent drop). Cleared by the next
    /// gesture that succeeds.
    custody_error: RefCell<Option<String>>,
}

/// The Devices sub-page's `error-message` text: a roster gesture's own error
/// first, else the standing enrollment refusal — a condition, not a gesture,
/// so it survives the snapshot repaint that clears a gesture error.
fn devices_error_text(
    ctx: &RenderCtx,
    snap: &fauna_devices_machine::DevicesSnapshot,
) -> Option<String> {
    snap.error
        .as_ref()
        .map(|err| err.resolve(strings::lookup))
        .or_else(|| ctx.custody_error.borrow().clone())
        .or_else(|| {
            ctx.enrollment_refusal
                .borrow()
                .map(|refusal| refusal.notice().to_string())
        })
}

/// Whether this app holds a pinned identity for its home nest (TOFU state,
/// never the nest's own claim) — with an offer a nest can hold, the condition for
/// the consent card's target select. A local read of the pin store.
fn nest_pinned(fauna_client: &FaunaClient) -> bool {
    fauna_anon_client::trust::pinned_nest_custodian_identity(&fauna_client.nest_rpc().nest_url())
        .is_some()
}

/// Paint (or clear) a custody gesture's error on the Devices `error-message`,
/// through `devices_error_text` so the machine's own error keeps precedence.
fn set_custody_error(ctx: &RenderCtx, machine: &DevicesMachine, error: Option<String>) {
    ctx.custody_error.replace(error);
    let text = devices_error_text(ctx, &machine.snapshot());
    crate::settings::render_error_label(&ctx.devices_error_label, text.as_deref());
}

/// Run one custody gesture from the Devices page through the shared
/// `fauna_client_custody::run_custody_act`, repaint from the re-folded facet,
/// and answer on `error-message` — never a silent drop (e2e convention 11).
fn run_custody_gesture(
    fauna_client: &Rc<FaunaClient>,
    ctx: &Rc<RenderCtx>,
    machine: &Arc<DevicesMachine>,
    view: &Rc<custody::CustodyView>,
    gesture: custody::CustodyGesture,
) {
    use custody::CustodyGesture as G;
    use fauna_client_custody::CustodyAct;
    let session = crate::conversations::conv_backend::active_session();
    let act = match gesture {
        G::MintOpen => {
            // The options are this account's 1:1 conversations (the request
            // travels over one). None → say so, rather than an empty picker
            // whose confirm can never succeed.
            let candidates = session
                .as_deref()
                .map(|s| fauna_client_custody::mint_candidates(s, fauna_client.secret_bytes()))
                .unwrap_or_default();
            if candidates.is_empty() {
                set_custody_error(
                    ctx,
                    machine,
                    Some(strings::devices::CUSTODY_MINT_NO_CONTACTS.to_string()),
                );
            } else {
                set_custody_error(ctx, machine, None);
                view.mint.open(candidates);
            }
            return;
        }
        G::SetBudget { grant_id, typed } => match fauna_core::format::parse_byte_size(&typed) {
            Some(cap) if cap > 0 => CustodyAct::SetBudget { grant_id, cap },
            // An unparseable budget makes no call — tui's answer, the
            // handle-validation shape.
            _ => {
                set_custody_error(
                    ctx,
                    machine,
                    Some(strings::backups::BACKUP_DESTINATION_CAPACITY_INVALID.to_string()),
                );
                return;
            }
        },
        G::Mint { host, channel_hex } => match <[u8; 32]>::try_from(host.as_slice()) {
            Ok(host) => CustodyAct::Mint {
                host: fauna_core::identity::ActorId(host),
                channel_hex,
            },
            Err(_) => {
                set_custody_error(
                    ctx,
                    machine,
                    Some(
                        "that person is not an eligible custodian — refresh and pick again"
                            .to_string(),
                    ),
                );
                return;
            }
        },
        G::Revoke { grant_id, holder } => CustodyAct::Revoke { grant_id, holder },
        G::Accept { grant_id, on_nest } => CustodyAct::Accept { grant_id, on_nest },
        G::Decline { grant_id } => CustodyAct::Decline { grant_id },
        G::Stop { grant_id } => CustodyAct::Stop { grant_id },
        G::Remove { grant_id } => CustodyAct::Remove { grant_id },
    };
    let is_mint = matches!(act, CustodyAct::Mint { .. });
    let act_ctx = fauna_client_custody::CustodyCtx {
        nest: Arc::clone(fauna_client.nest_rpc()),
        secret: fauna_client.secret_bytes(),
        // The store the budget and stop gestures write, and the re-fold's
        // registry-row overlay.
        store: crate::account_runtime::handle(),
        // The accept and the mint are POSTED by the drive over this session.
        session,
    };
    let ctx = Rc::clone(ctx);
    let machine = Arc::clone(machine);
    let view = Rc::clone(view);
    let client = Rc::clone(fauna_client);
    async_helper::spawn_with_snapshot(
        &fauna_client.runtime_handle(),
        move || fauna_client_custody::run_custody_act(act_ctx, act),
        move |(facet, error)| {
            // `None` = the re-fold could not read the config — keep the rows.
            if let Some(facet) = facet {
                view.paint(&facet, nest_pinned(&client));
            }
            if is_mint && error.is_none() {
                view.mint.close();
            }
            set_custody_error(&ctx, &machine, error);
        },
    );
}

/// The session's `DevicesMachine`, weakly — the E2E state serializer's one way
/// to the refresh barrier (`fauna_e2e_agent::DEVICES_REFRESHES_KEY`). Weak so a
/// settings shell torn down on sign-out or an actor switch takes its machine
/// with it rather than leaving the outgoing actor's counts on the state path.
fn current_machine_slot() -> &'static std::sync::Mutex<std::sync::Weak<DevicesMachine>> {
    static SLOT: std::sync::OnceLock<std::sync::Mutex<std::sync::Weak<DevicesMachine>>> =
        std::sync::OnceLock::new();
    SLOT.get_or_init(|| std::sync::Mutex::new(std::sync::Weak::new()))
}

/// The refresh triple of the session's `DevicesMachine`, or `None` before one
/// is built (`fauna_devices_machine::devices_refreshes_json` turns that into
/// the legitimate zero).
pub fn current_refresh_counts() -> Option<(u64, u64, u64)> {
    current_machine_slot()
        .lock()
        .unwrap()
        .upgrade()
        .map(|m| m.refresh_counts())
}

/// Wire the fleet-scope removal door (`devices.md` § Removing a Device) — the
/// shared adapter, unconditional: it reads this process's handle fresh per
/// call, and refuses the removal while there is none. Pulled out of
/// [`build_devices_and_folders_pages`] into a plain fn so a test can pin it
/// against a machine built without a GTK context.
fn wire_fleet_removal(machine: &DevicesMachine) {
    machine.set_fleet_removal(Arc::new(
        fauna_client_account_runtime::fleet_removal::RuntimeFleetRemoval::new(
            crate::account_runtime::handle,
        ),
    ));
    // This device's own peer-participation door (`p2p.md` § Per-device
    // participation), the same shared impl over the same handle read; when
    // the co-located agent holds the engine, the switch asks it for its pass
    // through the seat's holder nudge (`crate::sync_agent` publishes it).
    machine.set_p2p_participation_door(Arc::new(
        fauna_client_account_runtime::p2p_participation::RuntimeP2pParticipation::new(
            crate::account_runtime::handle,
        )
        .with_holder_nudge(
            fauna_client_account_runtime::p2p_participation::EngineHolderNudgeSlot::seat(),
        ),
    ));
}

/// Build the **Devices** + **Folders** sub-pages over one shared
/// `DevicesMachine`. Returns `(devices_page, folders_page, handles)` — each page
/// is a `gtk::ScrolledWindow` the settings shell adds to its sub-stack. Owns the
/// machine + observer-driven render loop + the embedded folder wizard; `app.rs`
/// only refreshes the machine (on auth + page-visible).
pub fn build_devices_and_folders_pages(
    fauna_client: &Rc<FaunaClient>,
) -> (
    gtk::ScrolledWindow,
    gtk::ScrolledWindow,
    DevicesFoldersHandles,
) {
    let dshell = build_devices_shell(fauna_client.actor_id().as_deref());
    let fshell = build_folders_shell(Some(fauna_client));

    let devices_page = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&dshell.content)
        .build();
    let folders_page = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&fshell.content)
        .build();

    // ── Machine + observer wiring ───────────────────────────────────────
    let (tx, rx) = crate::async_helper::snapshot_wake_channel();
    let observer: Arc<dyn DevicesObserver> = Arc::new(GtkDevicesObserver { tx });
    let machine = build_devices_machine(
        Arc::clone(fauna_client.nest_rpc()),
        Some(crate::account_runtime::folder_key_store()),
        observer,
    );
    *current_machine_slot().lock().unwrap() = Arc::downgrade(&machine);
    // Wire label custody (path-sealing S3) so the conflict list renders sealed
    // paths (`file-sync.md` § Sealed names & paths) — the same resolver +
    // owner key the Media page's byte download already uses (`client.rs`'s
    // own `label_custody` doc comment: a second resolver here could drift).
    machine.set_label_custody(fauna_client.label_custody());
    // Wire the B3 member-row join-filter: the centralized
    // `DevicesMachine` filter delegates to this per-app `MlsQuery` to drop
    // shared-with-me rows this client has not MLS-joined. Until wired the machine is
    // fail-safe (drops all member rows), so this MUST precede the first refresh for a
    // joined shared set to render.
    machine.set_mls_query(Arc::new(LinuxMlsQuery));
    wire_fleet_removal(&machine);
    // Wire the foreign-set (cross-nest) list source (Phase 2 client read-side):
    // sets shared from ANOTHER nest have no row in the own nest's list, so the
    // machine unions in the member's own `fauna.state.folder-keys` records (written at
    // share-accept). Best-effort — an unparsable secret just leaves foreign
    // rows unlisted (same fail-safe posture as the join-filter above).
    // Both sources take an owned `ActorKeypair`, which is deliberately NOT
    // `Clone` (it holds secret material), so each is derived from the secret hex
    // in its own arm rather than one derivation being handed round.
    match fauna_core::identity::ActorKeypair::from_secret_hex(fauna_client.secret_hex()) {
        Ok(_) => machine.set_foreign_sets_source(Arc::new(
            fauna_devices_machine::CustodyForeignSetsSource::new(
                crate::account_runtime::folder_key_store(),
            ),
        )),
        Err(e) => tracing::warn!("foreign-set list source not wired (bad secret): {e}"),
    }
    // Followed public folders (`ui/folders.md` § Following a public folder): a
    // follow lives entirely in the account's own `fauna.state.follows` rows
    // (the home nest keeps zero follower state), read through the account
    // store; while the store is not up the section keeps its last rows.
    machine.set_followed_folders_source(Arc::new(
        fauna_devices_machine::StoreFollowedFoldersSource::new(
            Arc::clone(fauna_client.nest_rpc()),
            crate::account_runtime::follows_seam(),
        ),
    ));

    // `folder-add-button` → open the embedded wizard. `open_wizard` seeds the
    // wizard's enrollable devices from the machine's own snapshot. Right after
    // opening, inject the user's global default conflict policy
    // (the `sync_prefs` record) so `submit()` stamps it onto the create — the
    // client-glue half of the Sync-defaults contract (file-sync.md §
    // Conflicts, policy). Best-effort: an unreadable record just leaves the
    // wizard un-injected (new set lands on the column default, auto).
    {
        let machine = Arc::clone(&machine);
        let rt = fauna_client.runtime_handle();
        fshell.folder_add_button.connect_clicked(move |_| {
            machine.open_wizard();
            let Some(wizard) = machine.wizard() else {
                return;
            };
            let store = crate::account_runtime::handle_source();
            async_helper::spawn_with_snapshot(
                &rt,
                move || async move { folders::load_default_conflict_policy(store).await },
                move |loaded| {
                    if let Ok(policy @ Some(_)) = loaded {
                        wizard.set_default_conflict_policy(policy);
                    }
                },
            );
        });
    }

    // Device-local folder map (seeded from an e2e injection if one was stashed
    // before the page existed, else the persisted map).
    let location_map: Rc<RefCell<Vec<LocationBinding>>> =
        Rc::new(RefCell::new(location_binding::initial_location_map()));

    // ── Render loop ─────────────────────────────────────────────────────
    // The follow form's two buttons need the authenticated client + the machine,
    // so they are wired here rather than at build time (the `folder-add-button`
    // idiom). Errors land on the Folders sub-page's own `error-message`.
    folders::wire_followed_section(
        &fshell.followed,
        &machine,
        fauna_client,
        &fshell.error_label,
    );

    let custody_view = Rc::clone(&dshell.custody);
    let ctx = Rc::new(RenderCtx {
        device_list_box: dshell.device_list_box,
        folder_list_box: fshell.folder_list_box.clone(),
        followed_list_box: fshell.followed.list.clone(),
        conflicts_group: fshell.conflicts_group,
        conflict_list_box: fshell.conflict_list_box,
        devices_error_label: dshell.error_label,
        folders_error_label: fshell.error_label,
        location_map: Rc::clone(&location_map),
        can_serve_webdav: Cell::new(false),
        own_tiers: RefCell::new(Vec::new()),
        #[cfg(feature = "p2p-share")]
        group_scopes: RefCell::new(Vec::new()),
        wizard_parent: fshell.content.upcast::<gtk::Widget>(),
        wizard_view: RefCell::new(None),
        local_device_id: crate::sync::device_id()
            .ok()
            .map(|d| fauna_core::hex32::encode(&d)),
        enrolled_device_row: RefCell::new(None),
        enrollment_refusal: RefCell::new(None),
        keyed_principals: RefCell::new(None),
        custody_error: RefCell::new(None),
    });
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let fauna_client = Rc::clone(fauna_client);
        crate::async_helper::spawn_wake_loop(rx, move || {
            render_pages(&machine, &ctx, &fauna_client);
            glib::ControlFlow::Continue
        });
    }

    // Register the e2e folder re-seed (see `inject_locations_for_test`): swap the
    // device-local map and re-render the folder list so each set's nested binding
    // rows repaint without the native folder picker.
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let fauna_client = Rc::clone(fauna_client);
        location_binding::register_rerender(Rc::new(move |injected: Vec<LocationBinding>| {
            *ctx.location_map.borrow_mut() = injected;
            let snap = machine.snapshot();
            folders::update_folder_list(
                &ctx.folder_list_box,
                &snap.folders,
                &machine,
                &fauna_client,
                &ctx.location_map,
                ctx.can_serve_webdav.get(),
                &ctx.own_tiers.borrow(),
                snap.website_address_enabled,
                #[cfg(feature = "p2p-share")]
                &ctx.group_scopes.borrow(),
            );
        }));
    }

    // Refresh the machine off WS-RPC whenever either sub-page becomes visible
    // (`connect_map` fires when a settings sub-stack child is shown) — replaces the
    // old per-nav HTTP read on the top-level Peers page. Neither page is the
    // settings stack's initial visible child (Status is), so this never fires
    // spuriously at construction — only on a real nav to Devices / Folders.
    for page in [&devices_page, &folders_page] {
        let machine = Arc::clone(&machine);
        let handle = fauna_client.runtime_handle();
        page.connect_map(move |_| {
            let machine = Arc::clone(&machine);
            handle.spawn(async move { machine.refresh().await });
        });
    }

    // The Devices sub-page's page-level reads (the custody facet, and the
    // account store's enrolled row / refusal / keyed set) run on EVERY nav
    // edge: mapping covers arriving from elsewhere, and the settings shell's
    // visible-child notify calls `devices_nav_refresh` for re-selecting the page
    // already showing, which re-notifies but never re-maps. A map-only read went
    // stale exactly there — the custody journey's owner never saw her row leave
    // pending.
    let devices_nav_hooks: Rc<RefCell<Vec<NavHook>>> = Rc::default();

    // T16 custody facet — its own hydrate on every nav to Devices. It is NOT
    // `DevicesMachine` state: the facet folds the `fauna.state.custody-ceremony` entries (plus the
    // account store's registry-row overlay), so it rides its own `connect_map`
    // beside the machine refresh above. The same edge fires one ceremony drive
    // pass (apple/android's load-edge shape): the ceremony sink registered with
    // the conversations session schedules one on every moved ceremony, and
    // this one is the crash-recovery edge.
    {
        let view = Rc::clone(&custody_view);
        {
            // Weak: the view holds the sink, so a strong capture would be a
            // cycle outliving the page.
            let weak = Rc::downgrade(&view);
            let ctx = Rc::clone(&ctx);
            let machine = Arc::clone(&machine);
            let fauna_client = Rc::clone(fauna_client);
            view.set_sink(Rc::new(move |gesture| {
                if let Some(view) = weak.upgrade() {
                    run_custody_gesture(&fauna_client, &ctx, &machine, &view, gesture);
                }
            }));
        }
        let fauna_client_outer = Rc::clone(fauna_client);
        devices_nav_hooks.borrow_mut().push(Rc::new(move || {
            let fauna_client = Rc::clone(&fauna_client_outer);
            let nest = Arc::clone(fauna_client.nest_rpc());
            let secret = fauna_client.secret_bytes();
            let store = crate::account_runtime::handle();
            fauna_client.runtime_handle().spawn({
                let nest = Arc::clone(&nest);
                let store = store.clone();
                async move {
                    fauna_client_custody::spawn_drive(
                        nest,
                        secret,
                        crate::conversations::conv_backend::active_session(),
                        store,
                    );
                }
            });
            let view = Rc::clone(&view);
            // All real I/O lives in `produce`, which runs on the tokio runtime;
            // the render closure only receives the result. Awaiting a
            // tokio-bound future inside `spawn_future_local` would panic the
            // task and silently render nothing (async_helper's own warning).
            async_helper::spawn_with_snapshot(
                &fauna_client.runtime_handle(),
                move || fauna_client_custody::load_custody_facet(nest, store),
                move |facet| {
                    // `None` = unreadable config this pass — keep whatever is
                    // painted rather than blanking live rows.
                    if let Some(facet) = facet {
                        view.paint(&facet, nest_pinned(&fauna_client));
                    }
                },
            );
        }));
    }

    // The "Shared with you" section's two knock lists — recipient-side folder
    // shares AND the co-present ceremony's consent cards — refresh whenever the
    // Folders sub-page becomes visible: a page-level read (not `DevicesMachine`
    // state), so it rides its own `connect_map` alongside the machine refresh
    // above. Only the Folders page carries the section. Mapping is one of two
    // edges: re-selecting the page that is already showing never re-maps it, so
    // the settings shell's visible-child notify covers the other.
    {
        let fauna_client = Rc::clone(fauna_client);
        folders_page.connect_map(move |_| {
            fauna_client.fetch_knock_lists();
        });
    }

    // Re-read the per-actor WebDAV serve capability whenever the Folders sub-page
    // becomes visible — like the pending-shares read above, a page-level read the
    // `DevicesMachine` doesn't own (it is keyless and cannot see `mail.msek`). On
    // nav, not once at build, so enabling mail elsewhere and coming back re-enables
    // the toggles without a restart. Re-renders only when the answer actually
    // changed, so a nav doesn't rebuild the list (which would collapse any expanded
    // row) for nothing.
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let fauna_client = Rc::clone(fauna_client);
        folders_page.connect_map(move |_| {
            let ctx = Rc::clone(&ctx);
            let machine = Arc::clone(&machine);
            let fauna_client = Rc::clone(&fauna_client);
            let query = fauna_client.can_serve_webdav();
            async_helper::spawn_with_snapshot(
                &fauna_client.runtime_handle(),
                move || query,
                move |can_serve| {
                    if ctx.can_serve_webdav.replace(can_serve) == can_serve {
                        return;
                    }
                    let snap = machine.snapshot();
                    folders::update_folder_list(
                        &ctx.folder_list_box,
                        &snap.folders,
                        &machine,
                        &fauna_client,
                        &ctx.location_map,
                        can_serve,
                        &ctx.own_tiers.borrow(),
                        snap.website_address_enabled,
                        #[cfg(feature = "p2p-share")]
                        &ctx.group_scopes.borrow(),
                    );
                },
            );
        });
    }

    // Re-read WHICH ROW THIS MACHINE ENROLLED ON whenever the Devices sub-page
    // becomes visible — the `device-this-mark-badge` match
    // (`devices.md` § This-device marker). Same page-level-read pattern as the
    // WebDAV capability below: the keyless `DevicesMachine` can't own it, and
    // the answer lives in the account runtime's registration latch. On nav
    // rather than once at build because the enrollment converges *after*
    // sign-in — a co-located agent provisioned by another app is discovered by
    // the runtime's own pass, so a build-time read would be `None` forever on
    // exactly the box the badge is wrong on. Re-renders only when the answer
    // changed, so a nav doesn't rebuild the list for nothing.
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let fauna_client = Rc::clone(fauna_client);
        devices_nav_hooks.borrow_mut().push(Rc::new(move || {
            let ctx = Rc::clone(&ctx);
            let machine = Arc::clone(&machine);
            let store = crate::account_runtime::handle();
            async_helper::spawn_with_snapshot(
                &fauna_client.runtime_handle(),
                move || async move {
                    match store {
                        // Two local reads off the store thread — no network,
                        // no IPC. An error reads as "cannot tell", which lands
                        // on the own-id fallback exactly like a fresh install
                        // (and paints no refusal — never a guessed notice).
                        Some(store) => (
                            store.enrolled_device_row().await.ok().flatten(),
                            store.enrollment_refusal().await.ok().flatten(),
                            // The keyless-posture marker's keyed set — also a
                            // local read; an error reads as "no resolved tip",
                            // which marks no row.
                            store.keyed_principals().await.ok().flatten(),
                        ),
                        None => (None, None, None),
                    }
                },
                move |(enrolled, refusal, keyed)| {
                    // The standing enrollment refusal (the tier device cap)
                    // paints on this sub-page's `error-message` while it
                    // stands, and comes down on the nav after the remedy
                    // landed. Repainted only on a change, like the badge.
                    if *ctx.enrollment_refusal.borrow() != refusal {
                        ctx.enrollment_refusal.replace(refusal);
                        let text = devices_error_text(&ctx, &machine.snapshot());
                        crate::settings::render_error_label(
                            &ctx.devices_error_label,
                            text.as_deref(),
                        );
                    }
                    if *ctx.enrolled_device_row.borrow() == enrolled
                        && *ctx.keyed_principals.borrow() == keyed
                    {
                        return;
                    }
                    ctx.enrolled_device_row.replace(enrolled);
                    ctx.keyed_principals.replace(keyed);
                    let this_row = mark_this_device_row(&machine, &ctx);
                    let snap = machine.snapshot();
                    roster::update_device_list(
                        &ctx.device_list_box,
                        &snap.devices,
                        &machine,
                        this_row.as_deref(),
                        ctx.keyed_principals.borrow().as_ref(),
                    );
                },
            );
        }));
    }
    let devices_nav_refresh: Rc<dyn Fn()> = {
        let hooks = Rc::clone(&devices_nav_hooks);
        Rc::new(move || {
            for hook in hooks.borrow().iter() {
                hook();
            }
        })
    };
    {
        let refresh = Rc::clone(&devices_nav_refresh);
        devices_page.connect_map(move |_| refresh());
    }

    // Re-read the creator's own subscription tiers whenever the Folders sub-page
    // becomes visible — the option set each website-enabled row's
    // `folder-paywall-tier-select` offers. Same page-level-read pattern as the
    // WebDAV capability above (the keyless `DevicesMachine` can't own it): fetched
    // off the key-bearing `FaunaClient`, re-rendering only when the tier set
    // actually changed so a nav doesn't rebuild the list (collapsing any expanded
    // row) for nothing (`folders.md` § Web paywall).
    {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let fauna_client = Rc::clone(fauna_client);
        folders_page.connect_map(move |_| {
            let ctx = Rc::clone(&ctx);
            let machine = Arc::clone(&machine);
            let fauna_client = Rc::clone(&fauna_client);
            let query = fauna_client.fetch_own_tier_names();
            async_helper::spawn_with_snapshot(
                &fauna_client.runtime_handle(),
                move || query,
                move |tiers| {
                    if *ctx.own_tiers.borrow() == tiers {
                        return;
                    }
                    ctx.own_tiers.replace(tiers);
                    let snap = machine.snapshot();
                    folders::update_folder_list(
                        &ctx.folder_list_box,
                        &snap.folders,
                        &machine,
                        &fauna_client,
                        &ctx.location_map,
                        ctx.can_serve_webdav.get(),
                        &ctx.own_tiers.borrow(),
                        snap.website_address_enabled,
                        #[cfg(feature = "p2p-share")]
                        &ctx.group_scopes.borrow(),
                    );
                },
            );
        });
    }

    // The co-present ceremony's shared-set rows repaint from the scopes half of
    // `GroupSharesLoaded` — the read `fetch_knock_lists` already runs on every
    // edge this page becomes visible on — never from a second read of their
    // own. A map-only second read went stale whenever the open page was
    // re-selected, which never re-maps, so a set admitted while the page was
    // showing never listed. A no-op when the scopes did not change, so a
    // re-read does not rebuild the list (collapsing an expanded row) for
    // nothing.
    #[cfg(feature = "p2p-share")]
    let repaint_group_scopes: RepaintGroupScopes = {
        let machine = Arc::clone(&machine);
        let ctx = Rc::clone(&ctx);
        let fauna_client = Rc::clone(fauna_client);
        Rc::new(move |scopes: &[crate::offline_share::GroupScopeView]| {
            if ctx.group_scopes.borrow().as_slice() == scopes {
                return;
            }
            ctx.group_scopes.replace(scopes.to_vec());
            let snap = machine.snapshot();
            folders::update_folder_list(
                &ctx.folder_list_box,
                &snap.folders,
                &machine,
                &fauna_client,
                &ctx.location_map,
                ctx.can_serve_webdav.get(),
                &ctx.own_tiers.borrow(),
                snap.website_address_enabled,
                &ctx.group_scopes.borrow(),
            );
        })
    };

    // The co-present ceremony's own state — built empty (matches tui's own
    // `OfflineShareState::default()`) and populated at the auth handler via
    // `crate::offline_share::init(secret_hex)` (mirrors tui's `session.rs`
    // post-auth hook); wiring the panel here, at build time, works regardless
    // because the click handlers close over the CELL, not a snapshot of its
    // contents. Shared with `app.rs`'s `DataMessage` handlers via the handle
    // below — the invitation-row accept/decline buttons need the same seat.
    #[cfg(feature = "p2p-share")]
    let offline_share_state = Rc::new(RefCell::new(
        crate::offline_share::OfflineShareState::default(),
    ));
    #[cfg(feature = "p2p-share")]
    folders::wire_offline_share_section(&fshell.offline_share, &offline_share_state, fauna_client);
    // Registers THIS window's reset with `actor_scope::reset_actor_scoped_state`
    // via `crate::offline_share::register_reset_hook` — the panel state lives
    // in this window's own `widgets` (a leaked authenticated-window widget
    // tree, `client.rs`'s `runtime` field docs), so the six teardown sites'
    // shared reset can only reach it through a registration. Overwrites
    // whatever the previous window registered; linux never exits on an actor
    // change, so exactly one window (and one registration) is ever live.
    #[cfg(feature = "p2p-share")]
    {
        let state = Rc::clone(&offline_share_state);
        let handles = fshell.offline_share.clone();
        crate::offline_share::register_reset_hook(move || {
            *state.borrow_mut() = crate::offline_share::OfflineShareState::default();
            folders::render_offline_share(&handles, &state.borrow().view());
        });
    }
    // The same registration, for the same unreachability: the Folders page's
    // ceremony read runs on `FaunaClient`, which cannot see this window's
    // panel state, and it needs the seat to answer a consent card while the
    // nest is gone (the group-share loader in `offline_share`; named
    // indirectly because a source-text test forbids its identifier here).
    #[cfg(feature = "p2p-share")]
    {
        let state = Rc::clone(&offline_share_state);
        crate::offline_share::register_seat_source(move || state.borrow().session_seat.clone());
    }

    let handles = DevicesFoldersHandles {
        folder_list_box: fshell.folder_list_box,
        pending_shares_group: fshell.pending_shares_group,
        pending_shares_box: fshell.pending_shares_box,
        #[cfg(feature = "p2p-share")]
        offline_share: fshell.offline_share,
        #[cfg(feature = "p2p-share")]
        share_transfer: fshell.share_transfer,
        #[cfg(feature = "p2p-share")]
        offline_share_state,
        #[cfg(feature = "p2p-share")]
        repaint_group_scopes,
        devices_machine: Arc::clone(&machine),
        devices_nav_refresh,
    };
    (devices_page, folders_page, handles)
}

/// The row this app marks as its own — the row it ENROLLED on, not the id it
/// happens to hold locally (`this_device_row` owns that rule and the reason
/// the fallback is load-bearing) — also handed to the machine as the
/// participation rule's last fallback, so the toggle's paint and arm agree
/// with `device-this-mark-badge` while the runtime has not yet named the
/// enrolled row (`p2p.md` § Per-device participation → *Which row is this
/// device's*). Called before every snapshot the roster paints from.
fn mark_this_device_row(machine: &DevicesMachine, ctx: &RenderCtx) -> Option<String> {
    let this_row = fauna_devices_machine::this_device_row(
        ctx.enrolled_device_row.borrow().as_deref(),
        ctx.local_device_id.as_deref(),
    );
    machine.set_this_device_row(this_row.clone());
    this_row
}

/// Re-render both sub-pages off a fresh `DevicesSnapshot`, and drive the wizard
/// dialog lifecycle (the wizard lives on the Folders sub-page).
fn render_pages(
    machine: &Arc<DevicesMachine>,
    ctx: &Rc<RenderCtx>,
    fauna_client: &Rc<FaunaClient>,
) {
    let this_row = mark_this_device_row(machine, ctx);
    let snap = machine.snapshot();
    roster::update_device_list(
        &ctx.device_list_box,
        &snap.devices,
        machine,
        this_row.as_deref(),
        ctx.keyed_principals.borrow().as_ref(),
    );
    folders::update_folder_list(
        &ctx.folder_list_box,
        &snap.folders,
        machine,
        fauna_client,
        &ctx.location_map,
        ctx.can_serve_webdav.get(),
        &ctx.own_tiers.borrow(),
        snap.website_address_enabled,
        #[cfg(feature = "p2p-share")]
        &ctx.group_scopes.borrow(),
    );
    folders::update_followed_list(
        &ctx.followed_list_box,
        &snap.followed,
        folders::unfollow_handler(machine, &ctx.folders_error_label),
    );
    conflicts::update_conflict_list(
        &ctx.conflicts_group,
        &ctx.conflict_list_box,
        &snap.conflicts,
        machine,
    );

    let text = snap.error.as_ref().map(|err| err.resolve(strings::lookup));
    // The Devices label additionally carries the standing enrollment refusal
    // (`devices_error_text`), which a gesture's snapshot must not clear while
    // the cap still binds; the Folders label paints the gesture error alone.
    let devices_text = devices_error_text(ctx, &snap);
    crate::settings::render_error_label(&ctx.devices_error_label, devices_text.as_deref());
    crate::settings::render_error_label(&ctx.folders_error_label, text.as_deref());

    // ── Wizard lifecycle ────────────────────────────────────────────────
    match snap.wizard.as_ref().map(|w| w.step) {
        Some(FolderWizardStep::Done) => {
            // Created — close the dialog, drop the wizard, refresh the list.
            if let Some(view) = ctx.wizard_view.borrow_mut().take() {
                view.dismiss();
            }
            machine.close_wizard();
            let machine = Arc::clone(machine);
            async_helper::run_on_tokio(async move { machine.refresh().await }, |_| {});
        }
        Some(_) => {
            // A wizard is open and mid-flow — ensure the dialog exists, then render
            // the current step off the embedded machine. `need_open` is bound first
            // so the immutable borrow is dropped before `borrow_mut`.
            let need_open = ctx.wizard_view.borrow().is_none();
            if need_open {
                let view = wizard::FolderWizardView::open(&ctx.wizard_parent, machine);
                *ctx.wizard_view.borrow_mut() = Some(view);
            } else if let Some(wiz) = machine.wizard()
                && let Some(view) = ctx.wizard_view.borrow().as_ref()
            {
                view.render(&wiz);
            }
        }
        None => {
            // No wizard — drop any open dialog (e.g. after a user dismiss).
            if let Some(view) = ctx.wizard_view.borrow_mut().take() {
                view.dismiss();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::testid::widget_names;

    struct NullObserver;
    impl DevicesObserver for NullObserver {
        fn on_changed(&self) {}
    }

    /// The page machine actually carries the fleet-removal door
    /// [`build_devices_and_folders_pages`] wires — not only that the shared
    /// adapter itself refuses correctly (`fleet_removal.rs`'s own pins).
    /// `crate::account_runtime::handle` answers `None` outside a running
    /// account runtime, so the door refuses unconditionally here too, no GTK
    /// context required.
    ///
    /// Mutation: drop the `wire_fleet_removal(&machine)` call in
    /// `build_devices_and_folders_pages` → `fleet_removal()` answers `None` →
    /// this pin reds (`cargo test -p fauna-linux --bins`).
    #[tokio::test]
    async fn the_page_machine_carries_the_fleet_removal_door() {
        let machine = DevicesMachine::new(
            Arc::new(NullObserver) as Arc<dyn DevicesObserver>,
            Arc::new(fauna_devices_machine::FakeDevicesNestApi::new()),
            Arc::new(fauna_devices_machine::FakeWizardFactory::new()),
        );
        wire_fleet_removal(&machine);
        let door = machine
            .fleet_removal()
            .expect("wire_fleet_removal must wire the door");
        let refusal = door
            .resolve_removal("aa", Some([0x11; 32]))
            .await
            .expect_err("no account runtime running in this process");
        assert!(matches!(
            refusal,
            fauna_devices_machine::FleetRemovalRefusal::Unavailable(_)
        ));
    }

    /// The **Devices** sub-page shell exposes the roster page's always-present
    /// ui.yaml IDs: `page-heading` (global-rule heading) and `error-message`
    /// (Rule 2). The dynamic `device-*` rows + AT-SPI discoverability are
    /// exercised by the cross-app E2E suite (`test_device_cards.py`).
    #[test]
    fn devices_shell_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let shell = build_devices_shell(None);
            let names = widget_names(&shell.content);
            for id in ["page-heading", "error-message"] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }

    /// The T16 custody section renders its ui.yaml family from the shared fold,
    /// and stays HIDDEN when the account has no custodians — an empty titled
    /// group reads as a feature that failed to load (`ui/devices.md` § Custody
    /// facet piece 2).
    ///
    /// Also pins the two rules a leg is most likely to get wrong: a **pending**
    /// ceremony has minted nothing to revoke, so its control is insensitive (a
    /// control that cannot succeed is not offered), and a **nest-anchored**
    /// custody belongs to the Nests page's `nest-trust-custody-*` family and
    /// must not appear here — one custody never renders in both.
    #[test]
    fn custody_section_renders_its_family_and_hides_when_empty() {
        use fauna_client_capabilities::view_model::{CustodyRowView, ReceiptState};

        fn row(pending: bool, nest: Option<&str>) -> CustodyRowView {
            CustodyRowView {
                grant_id: vec![1u8; 16],
                host: [7u8; 32],
                custodian_key: Some([9u8; 32]),
                custodian_nest_url: nest.map(str::to_string),
                scopes: None,
                lasts_until: None,
                liveness: None,
                receipt: None,
                receipt_state: ReceiptState::NoReceiptYet,
                pending,
            }
        }

        crate::testid::run_on_gtk_thread(|| {
            let shell = build_devices_shell(None);

            // Empty facet → the group is hidden and paints no card.
            custody::update_custody_list(
                &shell.custody.holder_group,
                &shell.custody.holder_list,
                &[],
                |_, _| {},
            );
            assert!(!shell.custody.holder_group.is_visible(), "empty → hidden");
            assert!(
                !widget_names(&shell.content)
                    .iter()
                    .any(|n| n == "custody-holder-card"),
                "no card for an account with no custodians",
            );

            // One device custody → the whole family renders.
            custody::update_custody_list(
                &shell.custody.holder_group,
                &shell.custody.holder_list,
                &[row(false, None)],
                |_, _| {},
            );
            assert!(
                shell.custody.holder_group.is_visible(),
                "populated → visible"
            );
            let names = widget_names(&shell.content);
            for id in [
                "custody-holder-card",
                "custody-holder-name",
                "custody-holder-receipt-status",
                "custody-holder-held-bytes",
                "custody-holder-revoke-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }

            // A nest-anchored custody renders on the NESTS page, never here.
            custody::update_custody_list(
                &shell.custody.holder_group,
                &shell.custody.holder_list,
                &[row(false, Some("https://friend-nest.example/"))],
                |_, _| {},
            );
            assert!(
                !shell.custody.holder_group.is_visible(),
                "a nest custodian is the Nests page's row, not this family",
            );
        });
    }

    /// Pieces 3 and the mint flow render every control LIVE (`devices.md`
    /// § Custody facet — never a card with dead controls) and each control
    /// raises its gesture carrying the row's GRANT ID: the budget input commits
    /// its typed text on activate, stop disables once stopped while remove
    /// stays available, the offer card carries the REQUIRED floor copy, the
    /// target select renders ONLY for an offer a nest can hold with a pinned
    /// nest (absent otherwise, never disabled) and picks the nest form, and the
    /// mint's confirm is live only once a real host is chosen and hands back
    /// that candidate's pair unchanged.
    #[test]
    fn custody_host_side_and_mint_render_live_controls() {
        use custody::CustodyGesture as G;
        use fauna_client_capabilities::custody_view::CustodyMintCandidateView;
        use fauna_client_capabilities::view_model::{
            CustodyFacetSnapshot, CustodyOfferView, HeldCustodyView,
        };
        use fauna_core::custody_grant::CustodyScopeSet;

        fn click(root: &gtk::Box, id: &str) {
            crate::testid::find_by_test_id(root, id)
                .unwrap_or_else(|| panic!("no {id}"))
                .downcast::<gtk::Button>()
                .unwrap()
                .emit_clicked();
        }

        let facet = |stopped: bool, nest_can_hold: bool| CustodyFacetSnapshot {
            rows: Vec::new(),
            held: vec![HeldCustodyView {
                grant_id: vec![0x11; 16],
                owner: [0xA0; 32],
                scopes: Some(CustodyScopeSet::Account),
                retained_bytes_cap: 8 * 1024 * 1024 * 1024,
                receipt: None,
                stopped,
            }],
            offers: vec![CustodyOfferView {
                grant_id: vec![0x22; 16],
                owner: [0xA1; 32],
                scopes: CustodyScopeSet::Account,
                offered_at_micros: 0,
                nest_can_hold,
            }],
        };

        crate::testid::run_on_gtk_thread(move || {
            let shell = build_devices_shell(None);
            let seen: Rc<RefCell<Vec<G>>> = Rc::default();
            {
                let seen = Rc::clone(&seen);
                shell
                    .custody
                    .set_sink(Rc::new(move |g| seen.borrow_mut().push(g)));
            }

            shell.custody.paint(&facet(false, true), false);
            assert!(shell.custody.held_group.is_visible());
            assert!(shell.custody.offer_group.is_visible());
            let names = widget_names(&shell.content);
            for id in [
                "custody-held-card",
                "custody-held-owner",
                "custody-held-scope",
                "custody-held-bytes",
                "custody-held-budget-input",
                "custody-held-stop-button",
                "custody-held-remove-button",
                "custody-offer-card",
                "custody-offer-floor-note",
                "custody-offer-accept-button",
                "custody-offer-decline-button",
                "custody-mint-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing {id:?}; have {names:?}"
                );
            }
            assert!(
                !names.iter().any(|n| n == "custody-offer-target-select"),
                "no pinned nest → the target select is absent, never disabled",
            );
            assert_eq!(
                crate::testid::find_by_test_id(&shell.content, "custody-offer-floor-note")
                    .unwrap()
                    .downcast::<gtk::Label>()
                    .unwrap()
                    .label(),
                strings::devices::CUSTODY_OFFER_FLOOR,
            );
            // The budget seeds from the shared seed text.
            let budget =
                crate::testid::find_by_test_id(&shell.content, "custody-held-budget-input")
                    .unwrap()
                    .downcast::<gtk::Entry>()
                    .unwrap();
            assert_eq!(
                budget.text(),
                fauna_core::format::byte_size(8 * 1024 * 1024 * 1024).resolve(strings::lookup),
            );
            budget.set_text("3072 MB");
            budget.emit_activate();
            click(&shell.content, "custody-held-stop-button");
            click(&shell.content, "custody-held-remove-button");
            click(&shell.content, "custody-offer-accept-button");
            click(&shell.content, "custody-offer-decline-button");
            assert_eq!(
                *seen.borrow(),
                vec![
                    G::SetBudget {
                        grant_id: vec![0x11; 16],
                        typed: "3072 MB".to_string(),
                    },
                    G::Stop {
                        grant_id: vec![0x11; 16]
                    },
                    G::Remove {
                        grant_id: vec![0x11; 16]
                    },
                    G::Accept {
                        grant_id: vec![0x22; 16],
                        on_nest: false,
                    },
                    G::Decline {
                        grant_id: vec![0x22; 16]
                    },
                ],
            );
            seen.borrow_mut().clear();

            // Stopped: stop disables, remove stays live. Advertised + pinned:
            // the select renders, and "My nest" makes the accept the nest form.
            shell.custody.paint(&facet(true, true), true);
            let stop =
                crate::testid::find_by_test_id(&shell.content, "custody-held-stop-button").unwrap();
            assert!(
                !stop.is_sensitive(),
                "a stopped hold has nothing further to stop"
            );
            assert!(
                crate::testid::find_by_test_id(&shell.content, "custody-held-remove-button")
                    .unwrap()
                    .is_sensitive(),
                "remove is the reclaim — always available",
            );
            crate::testid::find_by_test_id(&shell.content, "custody-offer-target-select")
                .expect("nest-holdable + pinned → the select renders")
                .downcast::<gtk::DropDown>()
                .unwrap()
                .set_selected(1);
            click(&shell.content, "custody-offer-accept-button");
            assert_eq!(
                *seen.borrow(),
                vec![G::Accept {
                    grant_id: vec![0x22; 16],
                    on_nest: true,
                }],
            );
            seen.borrow_mut().clear();

            // The mint: the button asks the page for candidates; the page opens
            // the flow over them.
            click(&shell.content, "custody-mint-button");
            assert_eq!(*seen.borrow(), vec![G::MintOpen]);
            seen.borrow_mut().clear();
            shell.custody.mint.open(vec![CustodyMintCandidateView {
                host: vec![0xB0; 32],
                channel_hex: "abcd".to_string(),
                label: "bob".to_string(),
            }]);
            let confirm =
                crate::testid::find_by_test_id(&shell.content, "custody-mint-confirm-button")
                    .unwrap();
            assert!(
                !confirm.is_sensitive(),
                "no host chosen → confirm cannot succeed"
            );
            assert!(
                crate::testid::find_by_test_id(&shell.content, "custody-mint-floor-note")
                    .unwrap()
                    .is_visible(),
                "the floor copy renders before the confirm",
            );
            crate::testid::find_by_test_id(&shell.content, "custody-mint-host-select")
                .unwrap()
                .downcast::<gtk::DropDown>()
                .unwrap()
                .set_selected(1);
            assert!(confirm.is_sensitive());
            click(&shell.content, "custody-mint-confirm-button");
            assert_eq!(
                *seen.borrow(),
                vec![G::Mint {
                    host: vec![0xB0; 32],
                    channel_hex: "abcd".to_string(),
                }],
            );
        });
    }

    // ── Following a public folder (phase 4 slice 4f-iii) ────────────────

    fn followed(name: &str, available: bool) -> fauna_devices_machine::FollowedFolderSummary {
        fauna_devices_machine::FollowedFolderSummary {
            folder_id: 7,
            home_nest_url: String::new(),
            owner_actor_id: "ab".repeat(32),
            display_name: name.to_string(),
            available,
            ..Default::default()
        }
    }

    /// The follow form is ARMED, not always painted — but the button that arms
    /// it always shows, because it is how a user gets their FIRST followed
    /// folder. Gating the button on a non-empty list would make the whole
    /// feature unreachable (`ui/folders.md` § Following a public folder, shape
    /// 1), which is the trap this pins.
    #[test]
    fn the_follow_form_is_armed_while_its_button_always_shows() {
        crate::testid::run_on_gtk_thread(|| {
            let shell = build_folders_shell(None);
            let names = widget_names(&shell.content);

            // With NO follows at all, the section and its button are present.
            assert!(
                names.iter().any(|n| n == "folder-follow-button"),
                "the follow button must show with an empty list; have {names:?}",
            );
            assert!(
                shell.followed.group.is_visible(),
                "the section is always offered, even with no follows",
            );

            // The two `optional_elements` ids are ARMED, not always painted:
            // they live inside the form, and the form starts closed.
            //
            // ⚠ Asserted on the FORM, not on each entry: GTK4's
            // `gtk_widget_get_visible` reports a widget's OWN flag, not its
            // effective visibility, so a child of a hidden box still answers
            // `true` — an assertion on the entries would pin nothing. (On
            // `gtk::Entry` it does not even resolve: `EntryExt::is_visible` is
            // the password-visibility flag, a different question entirely.)
            assert!(
                !shell.followed.form.is_visible(),
                "the follow form starts closed",
            );
            let in_form = widget_names(&shell.followed.form);
            for id in [
                "recipient-picker-input",
                "folder-follow-name-input",
                "folder-follow-confirm",
            ] {
                assert!(
                    in_form.iter().any(|n| n == id),
                    "{id:?} must live INSIDE the armed form, so closing the form \
                     withdraws it; form has {in_form:?}",
                );
            }
        });
    }

    /// A followed row renders its own family — and `available == false` is the
    /// **revoke**: the row STAYS, loudly, because a re-flip resumes it under the
    /// same `folder_id`. Dropping it silently is the bug this pins.
    #[test]
    fn a_followed_row_renders_its_family_and_keeps_an_unavailable_row() {
        crate::testid::run_on_gtk_thread(|| {
            let shell = build_folders_shell(None);

            folders::update_followed_list(
                &shell.followed.list,
                &[followed("holiday-pics", true)],
                |_, _| {},
            );
            let names = widget_names(&shell.content);
            for id in [
                "folder-followed-item",
                "folder-followed-status",
                "folder-unfollow-button",
            ] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }

            // The revoke keeps its row — and says so.
            folders::update_followed_list(
                &shell.followed.list,
                &[followed("holiday-pics", false)],
                |_, _| {},
            );
            let names = widget_names(&shell.content);
            assert!(
                names.iter().any(|n| n == "folder-followed-item"),
                "an unavailable follow keeps its row (the revoke is loud, not silent)",
            );
            assert!(
                names.iter().any(|n| n == "folder-unfollow-button"),
                "an unavailable follow can still be removed — that is the only \
                 action left on it",
            );
        });
    }

    /// The rows come off `snapshot.followed` and nothing else: an empty list
    /// paints no `folder-followed-item`, so a stale row can never outlive the
    /// follow it renders.
    #[test]
    fn an_empty_followed_list_paints_no_rows() {
        crate::testid::run_on_gtk_thread(|| {
            let shell = build_folders_shell(None);
            folders::update_followed_list(
                &shell.followed.list,
                &[followed("gone", true)],
                |_, _| {},
            );
            assert!(
                widget_names(&shell.content)
                    .iter()
                    .any(|n| n == "folder-followed-item"),
                "precondition: one row painted",
            );

            folders::update_followed_list(&shell.followed.list, &[], |_, _| {});
            assert!(
                !widget_names(&shell.content)
                    .iter()
                    .any(|n| n == "folder-followed-item"),
                "an unfollow's repaint must leave no row behind",
            );
        });
    }

    /// `peer-actor-id-copy-btn` is page-level (ui.yaml `indexed: false`) — it
    /// must exist in the shell BEFORE any device rows are populated, and stays
    /// a single instance regardless of roster size (device rows are populated
    /// later by `update_device_list`, not exercised by this shell-only test).
    #[test]
    fn devices_shell_exposes_the_page_level_actor_id_copy_button() {
        crate::testid::run_on_gtk_thread(|| {
            let shell = build_devices_shell(Some("ab".repeat(32).as_str()));
            let names = widget_names(&shell.content);
            assert_eq!(
                names
                    .iter()
                    .filter(|n| *n == "peer-actor-id-copy-btn")
                    .count(),
                1,
                "expected exactly one instance; have {names:?}",
            );
        });
    }

    /// The **Folders** sub-page shell exposes the control-plane page's
    /// always-present ui.yaml IDs: `page-heading`, `error-message`, and
    /// `folder-add-button`. The dynamic `folder-row` / `conflict-*` /
    /// `folder-location-*` / `folder-conflict-policy-select` rows are exercised by the
    /// cross-app E2E suite (`test_folders.py`, `test_sync_folders.py`,
    /// `test_devices_conflicts.py`).
    #[test]
    fn folders_shell_exposes_static_ui_yaml_ids() {
        crate::testid::run_on_gtk_thread(|| {
            let shell = build_folders_shell(None);
            let names = widget_names(&shell.content);
            for id in ["page-heading", "error-message", "folder-add-button"] {
                assert!(
                    names.iter().any(|n| n == id),
                    "missing widget id {id:?}; have {names:?}",
                );
            }
        });
    }
}

/// The "Shared with you" section is fetched on page-visible, never pushed, and
/// it holds TWO knock lists as one indexed `folder-pending-share` family: the
/// folder-share knocks and the co-present ceremony's consent cards
/// (`p2p.md` § Offline share initiation → *Built — the affordance, both roles*).
/// Both must re-read on every way the Folders page can become visible again.
///
/// Source guards, because the two nav edges are GTK signal wiring on a page
/// built from the whole settings shell, which no unit test here can construct.
/// The behavioural witness is `test_offline_share_two_seat.py --app linux`:
/// before these hooks, an invitation that arrived after sign-in never painted,
/// because the consent-card list was fetched once, at sign-in, and never
/// again.
#[cfg(test)]
mod knock_list_refresh_tests {
    /// The text of the first `signature` in `src` up to `end` — bounded so a
    /// needle cannot also match this module's own text further down a file.
    fn body<'a>(src: &'a str, signature: &str, end: &str) -> &'a str {
        let after = src
            .split_once(signature)
            .unwrap_or_else(|| panic!("`{signature}` is in the file"))
            .1;
        let stop = after
            .find(end)
            .unwrap_or_else(|| panic!("`{signature}` ends with {end:?}"));
        &after[..stop]
    }

    #[test]
    fn one_fetch_reads_both_knock_lists() {
        let fetch = body(
            include_str!("../../client.rs"),
            "pub fn fetch_knock_lists(&self)",
            "\n    }\n",
        );
        for list in [
            "self.fetch_folder_pending_shares()",
            "self.fetch_group_shares()",
        ] {
            assert!(
                fetch.contains(list),
                "`fetch_knock_lists` must read both lists the section shows; it does not call `{list}`"
            );
        }
    }

    #[test]
    fn the_folders_page_re_reads_both_knock_lists_whenever_it_maps() {
        let builder = body(
            include_str!("mod.rs"),
            "pub fn build_devices_and_folders_pages(",
            "\n}\n",
        );
        assert!(
            builder.contains("fauna_client.fetch_knock_lists();"),
            "the Folders page's map hook must re-read both knock lists, not only the folder-share one"
        );
        assert!(
            !builder.contains("fauna_client.fetch_folder_pending_shares();"),
            "the map hook reads one knock list on its own again — the consent cards would go stale"
        );
    }

    #[test]
    fn re_selecting_the_open_folders_page_re_reads_both_knock_lists() {
        // Re-selecting the page that is already showing never re-maps it:
        // `set_visible_child_forced` only re-notifies `visible-child-name`, so
        // the map hook above cannot cover this edge.
        let shell = body(
            include_str!("../settings_shell.rs"),
            "pub fn build_settings_shell(",
            "\n}\n",
        );
        let arm = shell
            .split_once("Some(\"folders\") =>")
            .expect("the shell's visible-child notify has a Folders arm")
            .1;
        let arm = &arm[..arm.find("\n            Some(").unwrap_or(arm.len())];
        assert!(
            arm.contains("fetch_knock_lists()"),
            "the Folders arm must re-read both knock lists; it reads: {arm}"
        );
    }

    /// The same edge on the Devices page: re-selecting it while it is showing
    /// must re-run the page-level reads (the custody facet and the account
    /// store's keyed set), or an accepted custody never leaves pending and a
    /// keyless device is never marked until the user navigates away and back.
    #[test]
    fn re_selecting_the_open_devices_page_re_runs_its_page_level_reads() {
        let shell = body(
            include_str!("../settings_shell.rs"),
            "pub fn build_settings_shell(",
            "\n}\n",
        );
        let arm = shell
            .split_once("Some(\"devices\") =>")
            .expect("the shell's visible-child notify has a Devices arm")
            .1;
        let arm = &arm[..arm.find("\n            Some(").unwrap_or(arm.len())];
        assert!(
            arm.contains("devices_nav_refresh()"),
            "the Devices arm must re-run the page-level reads; it reads: {arm}"
        );
    }

    /// The shared-set `folder-row`s paint from the scopes half of the SAME
    /// read that carries the consent cards, so they refresh on every edge the
    /// knock lists do. A map-only second read went stale whenever the open
    /// page was re-selected, and the co-present journey's recipient never
    /// listed the set it had just been admitted to.
    #[test]
    fn the_shared_sets_paint_from_the_read_that_carries_the_consent_cards() {
        let arm = body(
            include_str!("../../app.rs"),
            "DataMessage::GroupSharesLoaded { views: group_views } => {",
            "\n            }\n",
        );
        assert!(
            arm.contains("(widgets.repaint_group_scopes)(&group_views.scopes)"),
            "`GroupSharesLoaded` must repaint the shared-set rows from its scopes half; it reads: {arm}"
        );
        let builder = body(
            include_str!("mod.rs"),
            "pub fn build_devices_and_folders_pages(",
            "\n}\n",
        );
        assert!(
            !builder.contains("load_group_shares"),
            "the Folders page grew a second group-listing read of its own, which goes stale on re-select"
        );
    }

    /// A ceremony that lands a scope re-reads the listing at once, so the set
    /// lists on the page the user is looking at rather than at the next nav.
    #[test]
    fn a_ceremony_that_lands_a_scope_re_reads_the_listing() {
        let arm = body(
            include_str!("../../app.rs"),
            "DataMessage::OfflineShareProgressed { status } => {",
            "\n            }\n",
        );
        assert!(
            arm.contains("status.lands_a_scope()")
                && arm.contains("fauna_client.fetch_group_shares()"),
            "`OfflineShareProgressed` must re-read the group listing when a scope lands; it reads: {arm}"
        );
    }
}
