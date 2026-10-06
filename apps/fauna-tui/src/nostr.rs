//! The standalone **Nostr** page (`ui/nostr.md`).
//!
//! **Its own page, not a bridge row** (`nostr.md` § Page structure, ratified
//! 2026-06-13): Nostr gets the same treatment as mail — a first-class surface
//! rather than one entry on the unified Bridges page. On tui that surface is
//! the top-level `nostr-tab` sidebar row (`pages.rs::Page::Nostr`); the
//! settings-rail id (`{"view":"settings","id":"nostr"}`) is an **alias** onto
//! the same page, exactly as web renders one `NostrSettingsSection` from both
//! the `/app/nostr` route and its settings rail (`nostr.md:91`).
//!
//! **Where the logic lives** (`nostr.md` § Where logic lives): all of it is
//! already shared — `libs/fauna-bridge-nostr` (~20 NIPs) plus the nest's own
//! bridge worker. The control plane rides the **unified** `fauna.bridges.*`
//! wire keyed `bridge_id:"nostr"` (§ WS-RPC migration contract), reached here
//! through the shared typed `fauna_client_bridges::BridgesClient` — direct
//! Rust, no FFI hop, the same way linux consumes it (priority #2, the second
//! direct-Rust client). This page therefore composes **zero** Nostr protocol
//! logic of its own: it renders bridge status and posts bridge mutations.
//!
//! Shared surfaces do the only real computing on this page, and none is
//! re-implemented here: [`fauna_protocol::nostr_relay::relay_url_error`]
//! (the `wss://`/`ws://` check every app used to hand-roll, lifted
//! 2026-07-19, now also F7's private-network refusal, with its message), its
//! trim/dedup-then-append siblings
//! [`fauna_protocol::nostr_relay::trimmed_relay_input`] /
//! [`fauna_protocol::nostr_relay::relay_list_appending`] (lifted 2026-08-23),
//! and [`fauna_client_bridges::nostr_key_source_label`] (the
//! stored-`signing_mode` → localized label map, so every app's Signing Mode
//! row reads identically).
//!
//! **`registered` is NOT `available` — the trap this page must not fall into.**
//! `BridgeStatus::available` is the S8.9 *bridging* gate (`nostr.md` § The
//! bridging gate): false on a box where nobody has deposited an nsec — which is
//! **every fresh box**, including a fresh e2e nest. Gating the account-link form
//! on it hides the one control that can ever deposit the first nsec, so the box
//! can never bootstrap. That exact conflation shipped as a real bug on web and
//! apple (`nostr.md:191`, fixed 2026-07-14). Here the *unavailable* notice is
//! gated on [`NostrState::registered`] — whether the bridge appears in
//! `fauna.bridges.list` at all, i.e. whether the nest was built with the
//! `nostr` cargo feature — and `available` gates nothing.
//!
//! **Not on this page:** a DM list. A Nostr DM is a bridged room on unified
//! Conversations (`fauna.bridges.conversation.*`), on every app (`nostr.md`
//! § Implementation status today → DMs). Also absent: the NIP-07 link mode, a browser-extension boundary
//! call with no meaning in a terminal (§ Architectural rules 4 — native apps
//! offer generate / import / remote).
//!
//! The async split is the page-module contract's (`apps/tui.md` § The
//! page-module contract): [`apply_local`] lands the synchronous half and hands
//! back an [`Op`]; the agent's click path **awaits** the op and folds its
//! [`Outcome`] before replying (element reads are single-shot), while the
//! keyboard path spawns it and the outcome rides the `UiMessage` channel.
//! There is no shared *manager* behind this page (`BridgesClient` is a typed
//! transport wrapper, not a snapshot owner), so errors are written straight to
//! `App::errors` like the contacts page — no `sync_page_error` bridge is owed.

use fauna_ui_ids as ids;
use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_bridges::{
    BridgesClient, NOSTR_LINK_MODE_GENERATE as MODE_GENERATE,
    NOSTR_LINK_MODE_IMPORT as MODE_IMPORT, NOSTR_LINK_MODE_REMOTE as MODE_REMOTE,
    NOSTR_LINK_MODES as LINK_MODES, RELAY_LIST_KEY, bool_setting, nostr_content_toggle_options,
    nostr_key_source_label, nostr_link_mode_label, relay_list_setting,
};
use fauna_client_nostr::NostrBunkerClient;
#[cfg(feature = "zaps")]
use fauna_client_nostr::NostrZapSignerClient;
use fauna_client_nostr::nostr::BunkerAppEntry;
#[cfg(feature = "zaps")]
use fauna_client_nostr::nostr::ZapSignerEntry;
use fauna_i18n::strings::{common, nostr as t};
use fauna_protocol::Value as WireValue;
use fauna_protocol::bridges_ui::{BridgeFollow, BridgeStatus};
use fauna_sync_engine::account_runtime::AccountStoreHandle;
use serde_json::{Value, json};

use crate::app::App;
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

/// The bridge this page drives. Every `fauna.bridges.*` call on this page is
/// keyed by it (`nostr.md` § State & data shape).
pub const BRIDGE_ID: &str = "nostr";

// Link-*request* modes (`MODE_GENERATE`/`MODE_IMPORT`/`MODE_REMOTE`/`LINK_MODES`)
// are `fauna_client_bridges::NOSTR_LINK_MODE_*`/`NOSTR_LINK_MODES`, imported
// above under their local names — this page hand-copied them until this lift
// (`fauna-client-bridges/src/labels.rs`'s own doc comment has the full
// rationale, incl. why apple/android stay per-platform).

// ── State ────────────────────────────────────────────────────────────────────

/// The Nostr page's state, hung off [`App`]. Mirrors the *bridge* status rather
/// than inventing a page-local model: `nostr.md` § State & data shape resolved
/// (2026-06-08) that Nostr control-plane state rides the unified bridge
/// surface, superseding the standalone-`NostrSnapshot` proposal.
#[derive(Default)]
pub struct NostrState {
    /// The live WS-RPC channel, installed at the post-auth hook. `None`
    /// pre-auth — every reader degrades gracefully.
    pub nest: Option<Arc<NestClient>>,
    /// Whether the nostr bridge appears in `fauna.bridges.list` **at all**.
    /// False only on a nest built without the `nostr` cargo feature. This — and
    /// never `available` — is what gates the "unavailable" notice (module docs).
    pub registered: bool,
    /// The bridge's own status row, when registered.
    pub status: Option<BridgeStatus>,
    /// `fauna.bridges.list_follows` rows.
    pub follows: Vec<BridgeFollow>,
    /// The `nostr-link-mode` picker's selection (a link *request* mode).
    pub link_mode: String,
    /// The `nostr-nsec-input` buffer (import mode only). Transient by
    /// construction: cleared the moment a link succeeds, so the pasted key
    /// never outlives the request that consumed it (`nostr.md` § Persistence —
    /// the steady-state home is the nest, never per-app storage).
    pub nsec_input: String,
    /// The remote-signer (NIP-46 bunker) URL buffer, remote mode only. Native
    /// apps all carry this field with **no ui.yaml id** — linux and apple
    /// do the same (`nostr.md:93`), so tui registers no element for it either.
    pub bunker_url_input: String,
    /// The `nostr-relay-input` buffer.
    pub relay_input: String,
    /// The `nostr-follow-pubkey-input` buffer.
    pub follow_pubkey_input: String,
    /// The `nostr-follow-petname-input` buffer.
    pub follow_petname_input: String,
    /// The NIP-46 *Connected apps* roster (`fauna.nostr.bunker.list`).
    pub bunker_apps: Vec<BunkerAppEntry>,
    /// The **one-time** `bunker://…` connect string, held only between minting
    /// an invite and the next refresh. `nostr.md:52` calls this a one-time
    /// reveal, and the secret it carries is single-use — so it lives in a field
    /// that any subsequent load clears, never in the roster row.
    pub bunker_connect_string: Option<String>,
    /// The NIP-57 zap-signer trust root (`fauna.nostr.zap_signers.list`) — the
    /// signer pubkeys this payee believes. **Empty is a meaningful state**, not
    /// a loading gap: a payee who has designated nobody believes nobody, which
    /// is the ratified out-of-the-box default (`monetization.md` § Zap receipts
    /// — the trust model).
    #[cfg(feature = "zaps")]
    pub zap_signers: Vec<ZapSignerEntry>,
    /// The `nostr-zap-signer-pubkey-input` buffer.
    #[cfg(feature = "zaps")]
    pub zap_signer_pubkey_input: String,
    /// The `nostr-zap-signer-label-input` buffer.
    #[cfg(feature = "zaps")]
    pub zap_signer_label_input: String,
    /// The Dim-3 courtesy read (`fauna_client_features`) — whether the `zaps`
    /// plane's policy currently admits a *designation*. `None` until the first
    /// load resolves; see [`designate_gate`] for why an un-hydrated read leaves
    /// the button live.
    #[cfg(feature = "zaps")]
    pub features: Option<Vec<fauna_client_features::FeatureRow>>,
    /// Whether the successor is owed an npub confirmation right now
    /// (`fauna_client_config::npub_confirmation_owed_for`) — refreshed at nav
    /// entry only (module docs on [`Op::Refresh`]), not after every ordinary
    /// mutation: nothing except the ceremony itself or the confirm gesture can
    /// change the answer within a session.
    pub npub_confirmation_owed: bool,
}

impl NostrState {
    /// Whether the account is linked. The e2e `is_linked()` signal is
    /// `nostr-pubkey-copy-btn`'s visibility, so this predicate and that
    /// element's gate must stay the same one expression — linux shipped a bug
    /// precisely because its rows rendered unconditionally (`nostr.md:201`).
    pub fn linked(&self) -> bool {
        self.status.as_ref().is_some_and(|s| s.linked)
    }

    /// The link *request* mode to render and submit.
    ///
    /// Falls back to `generate` when the buffer is empty. `init` seeds it, but
    /// `Default` cannot (it yields an empty `String`), and the cross-app
    /// action layer's `link_generate()` deliberately clicks
    /// `nostr-link-button` **without** touching the picker — "the mode defaults
    /// to generate, so no mode select is needed". An empty mode would post an
    /// empty `fauna.bridges.link` mode and fail, so the default lives at the
    /// read edge where every caller shares it rather than in one constructor.
    pub fn effective_link_mode(&self) -> &str {
        if self.link_mode.is_empty() {
            MODE_GENERATE
        } else {
            self.link_mode.as_str()
        }
    }

    /// The stored `signing_mode` the bridge reports, if any.
    pub fn signing_mode(&self) -> Option<&str> {
        self.status.as_ref()?.mode.as_deref()
    }

    /// Whether the linked account is **custodial** (`generated`/`imported`) —
    /// the dimension the NIP-46 *Connected apps* section is gated on: a
    /// `remote`/`nip07` account has no key on the box to sign with, so the
    /// bunker role is never offered (`nostr.md:50`).
    pub fn custodial(&self) -> bool {
        matches!(self.signing_mode(), Some("generated") | Some("imported"))
    }

    /// The npub for the pubkey row — it rides `identity.value` on the generic
    /// bridge wire (`nostr.md:186`).
    pub fn npub(&self) -> Option<&str> {
        self.status
            .as_ref()?
            .identity
            .as_ref()
            .map(|i| i.value.as_str())
    }

    /// The configured relay URLs, parsed from the JSON-string `relay_list`
    /// setting. Absent/blank/malformed all read as "no explicit list" — the
    /// nest falls back to its own `DEFAULT_RELAYS` in that case, so degrading
    /// to empty is correct rather than merely safe.
    pub fn relays(&self) -> Vec<String> {
        let Some(status) = &self.status else {
            return Vec::new();
        };
        relay_list_setting(&status.settings)
    }

    /// A content flag's current value, defaulted per the shared catalog
    /// [`nostr_content_toggle_options`].
    pub fn toggle(&self, key: &str, default: bool) -> bool {
        match &self.status {
            Some(s) => bool_setting(&s.settings, key, default),
            None => default,
        }
    }
}

/// The wire payload persisting a relay list — the JSON-string shape above.
fn relay_list_payload(relays: &[String]) -> WireValue {
    let json = serde_json::to_string(relays).unwrap_or_else(|_| "[]".to_string());
    WireValue::Map(BTreeMap::from_iter([(
        RELAY_LIST_KEY.to_string(),
        WireValue::String(json),
    )]))
}

/// A follow row's display text. `list_follows` re-encodes a stored hex pubkey
/// to an npub, so the id is already display-ready; a petname prefixes it when
/// the user set one. (The cross-app e2e asserts on row COUNT, not text —
/// `actions/nostr.py::add_follow` — so this is paint, not contract.)
fn follow_display(f: &BridgeFollow) -> String {
    match f.petname.as_deref() {
        Some(p) if !p.is_empty() => format!("{p} — {}", f.id),
        _ => f.id.clone(),
    }
}

/// Build the page state at the post-auth hook. Deliberately does **not** kick a
/// fetch: entering the tab is the trigger ([`nav_enter_op`], awaited on the nav
/// edge), so a login never pays for a page the user may not open — the same
/// posture the Media page takes.
///
/// The succession-aftermath npub confirm reads and writes the account plane
/// (`fauna.state.nostr-confirmation`) through the app's account-store handle
/// (`SettingsState::account_store`), taken at op-build time because the
/// runtime lands after this hook — `None` until it does, in which case the
/// read degrades to "not owed" like any other best-effort leg of the
/// aftermath.
pub fn init(nest: Arc<NestClient>) -> NostrState {
    NostrState {
        nest: Some(nest),
        link_mode: MODE_GENERATE.to_string(),
        ..NostrState::default()
    }
}

/// The refetch entering this tab implies — the page's leg of the one nav-edge
/// hook (`crate::app::on_nav_enter`). Returns the op; the caller runs it (the
/// agent awaits, the keyboard spawns), never a fire-and-forget spawn that could
/// race a same-visit mutation.
pub fn nav_enter_op(app: &App) -> Option<Op> {
    Some(Op::Refresh {
        nest: app.nostr.nest.clone()?,
        account: app.settings.account_store.clone(),
    })
}

// ── Field access ─────────────────────────────────────────────────────────────

/// A Nostr-page editable field. Every one is a local buffer committed by an
/// explicit button, never per keystroke.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum NostrField {
    /// `nostr-nsec-input` — the imported nsec (import mode only).
    Nsec,
    /// The remote-signer bunker URL (remote mode only; no ui.yaml id, matching
    /// linux/apple/android).
    BunkerUrl,
    /// `nostr-relay-input` — the new relay URL.
    Relay,
    /// `nostr-follow-pubkey-input` — npub or raw hex pubkey.
    FollowPubkey,
    /// `nostr-follow-petname-input` — the optional petname.
    FollowPetname,
    /// `nostr-zap-signer-pubkey-input` — the signer's 64-hex pubkey.
    #[cfg(feature = "zaps")]
    ZapSignerPubkey,
    /// `nostr-zap-signer-label-input` — the optional signer label.
    #[cfg(feature = "zaps")]
    ZapSignerLabel,
}

pub fn field(state: &NostrState, field: &NostrField) -> String {
    match field {
        NostrField::Nsec => state.nsec_input.clone(),
        NostrField::BunkerUrl => state.bunker_url_input.clone(),
        NostrField::Relay => state.relay_input.clone(),
        NostrField::FollowPubkey => state.follow_pubkey_input.clone(),
        NostrField::FollowPetname => state.follow_petname_input.clone(),
        #[cfg(feature = "zaps")]
        NostrField::ZapSignerPubkey => state.zap_signer_pubkey_input.clone(),
        #[cfg(feature = "zaps")]
        NostrField::ZapSignerLabel => state.zap_signer_label_input.clone(),
    }
}

pub fn set_field(state: &mut NostrState, field: NostrField, value: String) {
    match field {
        NostrField::Nsec => state.nsec_input = value,
        NostrField::BunkerUrl => state.bunker_url_input = value,
        NostrField::Relay => state.relay_input = value,
        NostrField::FollowPubkey => state.follow_pubkey_input = value,
        NostrField::FollowPetname => state.follow_petname_input = value,
        #[cfg(feature = "zaps")]
        NostrField::ZapSignerPubkey => state.zap_signer_pubkey_input = value,
        #[cfg(feature = "zaps")]
        NostrField::ZapSignerLabel => state.zap_signer_label_input = value,
    }
}

// ── Gestures ─────────────────────────────────────────────────────────────────

/// A gesture on the Nostr page. Each maps onto one `fauna.bridges.*` call or a
/// local buffer change — never a navigation decision of its own.
#[derive(Debug, Clone)]
pub enum Action {
    /// `nostr-link-mode` — pick a link *request* mode. Purely local: it swaps
    /// which credential field renders, and is read at submit.
    SetLinkMode(String),
    /// `nostr-link-button` — `fauna.bridges.link`, then refresh.
    Link,
    /// `nostr-unlink-button` — `fauna.bridges.unlink`, then refresh.
    Unlink,
    /// `nostr-pubkey-copy-btn` — copy the npub (OSC 52). Client glue, the one
    /// per-app affordance `nostr.md` § User actions marks as such.
    CopyPubkey,
    /// One of the 5 content flags — `set_settings` with just that key (the nest
    /// merges by key, so a partial blob never clobbers `relay_list`), then
    /// refresh so the painted switch reflects the *persisted* value.
    ToggleContent { key: String, value: bool },
    /// `nostr-add-relay` — validate, read-modify-write `relay_list`, refresh.
    AddRelay,
    /// `nostr-remove-relay[i]` — drop row `i` from `relay_list`, refresh.
    RemoveRelay { index: usize },
    /// `nostr-add-follow` — `fauna.bridges.add_follow`, then re-list.
    AddFollow,
    /// `nostr-remove-follow[i]` — `fauna.bridges.remove_follow`, then re-list.
    RemoveFollow { id: String },
    /// `nostr-bunker-connect-btn` — mint a NIP-46 invite, revealing the
    /// one-time `bunker://…` string and creating a pending roster row.
    ConnectApp,
    /// `nostr-bunker-connect-copy-btn` — copy the revealed connect string.
    CopyConnectString,
    /// `nostr-zap-signer-add-btn` — designate a signer
    /// (`fauna.nostr.zap_signers.add`), then re-list. The pubkey is sent as the
    /// user typed it: the nest validates 64-hex and normalizes to lowercase,
    /// and only the STORED form ever matches a receipt.
    #[cfg(feature = "zaps")]
    AddZapSigner,
    /// `nostr-zap-signer-remove[i]` — stop trusting one signer
    /// (`fauna.nostr.zap_signers.remove`), keyed by the row's own stored
    /// pubkey so a roster that shifted under us still removes what the user saw.
    #[cfg(feature = "zaps")]
    RemoveZapSigner { pubkey: String },
    /// `nostr-npub-confirm-yes-button` — "yes, that's my npub": writes
    /// the confirmation stamp at `now()` to the account plane
    /// (`fauna.state.nostr-confirmation`), then refreshes (the banner disappears because the fresh read says the
    /// confirmation is no longer owed — non-optimistic, like every other
    /// mutation on this page).
    ConfirmNpub,
    /// `nostr-npub-confirm-no-button` — "no / nothing is linked": routes into
    /// the *existing* new-key path rather than a bespoke one (`nostr.md`:75 —
    /// "the remedy is the existing page machinery"). Unlinking is that
    /// machinery's own entry point: it drops the wrong (or thief-relinked)
    /// key and reveals the link form, where the owner generates or imports a
    /// fresh one exactly as any first-time link would.
    DismissNpubToNewKey,
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback
    /// arm, so a new Nostr gesture must answer the offline question.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            Action::Link => Some("fauna.bridges.link"),
            Action::Unlink => Some("fauna.bridges.unlink"),
            // The content flags, the relay list and the follow list all
            // persist through the one unified `set_settings` call
            // (`relay_list_payload` is just its payload shape).
            Action::ToggleContent { .. } | Action::AddRelay | Action::RemoveRelay { .. } => {
                Some("fauna.bridges.set_settings")
            }
            Action::AddFollow => Some("fauna.bridges.add_follow"),
            Action::RemoveFollow { .. } => Some("fauna.bridges.remove_follow"),
            Action::ConnectApp => Some("fauna.nostr.bunker.create_invite"),
            #[cfg(feature = "zaps")]
            Action::AddZapSigner => Some("fauna.nostr.zap_signers.add"),
            #[cfg(feature = "zaps")]
            Action::RemoveZapSigner { .. } => Some("fauna.nostr.zap_signers.remove"),
            // The confirm write is a local account-plane put
            // (`fauna.state.nostr-confirmation`) the runtime publishes later —
            // no wire call at the gesture, so nothing for the offline gate.
            Action::ConfirmNpub => None,
            // "No / nothing is linked" IS an unlink — the existing new-key path
            // is reached through the existing unlink+relink machinery, not a
            // bespoke one (`nostr.md`:75).
            Action::DismissNpubToNewKey => Some("fauna.bridges.unlink"),
            // Local: the link-mode radio writes a buffer, and both copies are
            // clipboard writes over data already on screen.
            Action::SetLinkMode(_) | Action::CopyPubkey | Action::CopyConnectString => None,
        }
    }
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let account = app.settings.account_store.clone();
    let st = &mut app.nostr;
    match action {
        Action::SetLinkMode(mode) => {
            st.link_mode = mode;
            None
        }
        Action::Link => {
            let nest = st.nest.clone()?;
            let mode = st.effective_link_mode().to_string();
            let mut params = BTreeMap::new();
            if mode == MODE_IMPORT {
                let nsec = st.nsec_input.trim().to_string();
                if nsec.is_empty() {
                    // A client-glue validation failure — written straight to
                    // `App::errors`, the page-module contract's rule for an
                    // error shared Rust never saw.
                    app.errors
                        .insert(Page::Nostr, t::link_account::ENTER_NSEC.to_string());
                    return None;
                }
                params.insert("nsec".to_string(), WireValue::String(nsec));
            } else if mode == MODE_REMOTE {
                params.insert(
                    "bunker_url".to_string(),
                    WireValue::String(st.bunker_url_input.trim().to_string()),
                );
            }
            app.errors.remove(&Page::Nostr);
            Some(Op::Link {
                nest,
                account,
                mode,
                params: WireValue::Map(params),
            })
        }
        Action::Unlink => Some(Op::Unlink {
            nest: st.nest.clone()?,
        }),
        Action::CopyPubkey => {
            if let Some(npub) = st.npub() {
                crate::wizard::copy_to_clipboard(npub);
            }
            None
        }
        Action::ToggleContent { key, value } => Some(Op::SetSettings {
            nest: st.nest.clone()?,
            settings: WireValue::Map(BTreeMap::from_iter([(key, WireValue::Bool(value))])),
        }),
        Action::AddRelay => {
            let nest = st.nest.clone()?;
            // Shared trim/empty rule (matches web/apple/android/linux's
            // silent no-op on a blank submit — tui and windows previously
            // fell through unfiltered to the invalid-url error below).
            let url = fauna_protocol::nostr_relay::trimmed_relay_input(&st.relay_input)?;
            // The ONE shared predicate and its message — never a hand-rolled
            // prefix check (`nostr.md` § Where logic lives; lifted 2026-07-19
            // after four apps each grew their own copy). It refuses a
            // malformed URL and, per F7, a private-network relay.
            if let Some(err) = fauna_protocol::nostr_relay::relay_url_error(&url) {
                app.errors
                    .insert(Page::Nostr, crate::wizard::localized(&err));
                return None;
            }
            let relays = st.relays();
            // Shared dedup-then-append rule — every app's identical silent
            // no-op on an exact-string duplicate (`nostr_relay.rs`).
            let Some(relays) = fauna_protocol::nostr_relay::relay_list_appending(&relays, &url)
            else {
                st.relay_input.clear();
                return None;
            };
            st.relay_input.clear();
            app.errors.remove(&Page::Nostr);
            Some(Op::SetSettings {
                nest,
                settings: relay_list_payload(&relays),
            })
        }
        Action::RemoveRelay { index } => {
            let nest = st.nest.clone()?;
            let mut relays = st.relays();
            if index >= relays.len() {
                return None;
            }
            relays.remove(index);
            Some(Op::SetSettings {
                nest,
                settings: relay_list_payload(&relays),
            })
        }
        Action::AddFollow => {
            let nest = st.nest.clone()?;
            let id = st.follow_pubkey_input.trim().to_string();
            if id.is_empty() {
                return None;
            }
            let petname = {
                let p = st.follow_petname_input.trim();
                (!p.is_empty()).then(|| p.to_string())
            };
            st.follow_pubkey_input.clear();
            st.follow_petname_input.clear();
            Some(Op::AddFollow { nest, id, petname })
        }
        Action::RemoveFollow { id } => Some(Op::RemoveFollow {
            nest: st.nest.clone()?,
            id,
        }),
        Action::ConnectApp => Some(Op::CreateInvite {
            nest: st.nest.clone()?,
        }),
        Action::CopyConnectString => {
            if let Some(s) = &st.bunker_connect_string {
                crate::wizard::copy_to_clipboard(s);
            }
            None
        }
        #[cfg(feature = "zaps")]
        Action::AddZapSigner => {
            let pubkey = st.zap_signer_pubkey_input.trim().to_string();
            // Client-glue validation, written straight to `App::errors` like the
            // relay check above: the nest refuses a non-64-hex key anyway, so
            // this only spares a guaranteed round trip and names the rule.
            if pubkey.len() != 64 || !pubkey.chars().all(|c| c.is_ascii_hexdigit()) {
                app.errors
                    .insert(Page::Nostr, t::zap_signers::INVALID_PUBKEY.to_string());
                return None;
            }
            // Deliberately NOT blocked as a duplicate, unlike the relay list
            // above: `fauna.nostr.zap_signers.add` is idempotent and a re-add
            // REFRESHES the label, so re-entering a designated key is the only
            // rename path there is (no `set_label` affordance exists on any
            // app). Blocking it would deny a rename to spare a round trip, and
            // the user sees the roster row change either way — never the silent
            // no-op the relay check exists to prevent.
            let label = st.zap_signer_label_input.trim().to_string();
            // Only now — the verdict above is about the INPUT, so it must be
            // reachable whether or not a nest handle exists yet.
            let nest = st.nest.clone()?;
            st.zap_signer_pubkey_input.clear();
            st.zap_signer_label_input.clear();
            app.errors.remove(&Page::Nostr);
            Some(Op::AddZapSigner {
                nest,
                pubkey,
                label,
            })
        }
        #[cfg(feature = "zaps")]
        Action::RemoveZapSigner { pubkey } => Some(Op::RemoveZapSigner {
            nest: st.nest.clone()?,
            pubkey,
        }),
        Action::ConfirmNpub => Some(Op::ConfirmNpub {
            nest: st.nest.clone()?,
            account: account?,
        }),
        // Reuses the existing unlink gesture's own Op — "the existing page
        // machinery", not a bespoke one (`nostr.md`:75).
        Action::DismissNpubToNewKey => Some(Op::Unlink {
            nest: st.nest.clone()?,
        }),
    }
}

// ── Network half ─────────────────────────────────────────────────────────────

/// The network half of a Nostr gesture — owns only `Arc`s + owned data, so it
/// can be awaited on the agent's path or spawned on the keyboard's.
pub enum Op {
    /// `fauna.bridges.list` + `list_follows` (nav edge only — the other
    /// variants below call the plain [`refresh`] helper directly after their
    /// own mutation, never this variant). Additionally checks the npub
    /// confirmation (`fauna.recovery.succession.status` + a local
    /// `fauna.state.nostr-confirmation` read) — the one thing this variant
    /// does that a mutation-triggered refresh does not, since nothing but the
    /// confirm gesture or a fresh succession can change that answer within a
    /// session (module docs on [`NostrState::npub_confirmation_owed`]).
    Refresh {
        nest: Arc<NestClient>,
        account: Option<AccountStoreHandle>,
    },
    /// `fauna.bridges.link`, then — best-effort — a confirm write: a fresh
    /// link (first-ever, or after "no / nothing is linked" unlinked the old
    /// key) is itself the new-npub remedy nostr.md:75 describes, since the
    /// owner just chose this key. Harmless on an account with no succession
    /// history: the predicate short-circuits before ever reading it.
    Link {
        nest: Arc<NestClient>,
        account: Option<AccountStoreHandle>,
        mode: String,
        params: WireValue,
    },
    Unlink {
        nest: Arc<NestClient>,
    },
    /// A partial settings blob. The nest merges by key
    /// (`nostr/db.rs::update_settings` — each field a separate gated `UPDATE`),
    /// so sending one key never clobbers the others.
    SetSettings {
        nest: Arc<NestClient>,
        settings: WireValue,
    },
    AddFollow {
        nest: Arc<NestClient>,
        id: String,
        petname: Option<String>,
    },
    RemoveFollow {
        nest: Arc<NestClient>,
        id: String,
    },
    /// `fauna.nostr.bunker.create_invite` — mints a pending connection and
    /// returns the one-time `bunker://…` string.
    CreateInvite {
        nest: Arc<NestClient>,
    },
    /// `fauna.nostr.zap_signers.add` — designate a signer, then refresh.
    ///
    /// This is a **wired gate surface** (`zaps.signer.designate`,
    /// `dynamic-features.md` § per-surface composition), so the nest may refuse
    /// it with a typed `fauna.features.denied` / `.over_quota`. The refusal
    /// lands on `error-message` like any other; [`designate_gate`] is the
    /// courtesy layer that keeps the user from reaching it in the first place.
    #[cfg(feature = "zaps")]
    AddZapSigner {
        nest: Arc<NestClient>,
        pubkey: String,
        label: String,
    },
    /// `fauna.nostr.zap_signers.remove` — stop trusting one signer.
    ///
    /// Deliberately **ungated** nest-side: de-escalation is never gated, because
    /// a tier that can only tighten must never trap a user in a configuration
    /// they can no longer undo (`dynamic-features.md`, ruling (i)). So this verb
    /// gets no courtesy layer either — its button is always live.
    #[cfg(feature = "zaps")]
    RemoveZapSigner {
        nest: Arc<NestClient>,
        pubkey: String,
    },
    /// The account-plane confirm write, then re-check + refresh (the
    /// same nav-enter path, so the banner's disappearance is a fresh read
    /// rather than an assumed outcome — non-optimistic like every other
    /// mutation here).
    ConfirmNpub {
        nest: Arc<NestClient>,
        account: AccountStoreHandle,
    },
}

/// What an op resolved to.
#[derive(Debug)]
pub enum Outcome {
    /// Fresh bridge status + follows. Every successful op ends here — the page
    /// is non-optimistic by construction, so a painted switch or row always
    /// reflects what the nest actually persisted, never the tap.
    Loaded {
        registered: bool,
        status: Option<Box<BridgeStatus>>,
        follows: Vec<BridgeFollow>,
        bunker_apps: Vec<BunkerAppEntry>,
        /// The zap-signer trust root. An empty vec is the meaningful
        /// designated-nobody state, never "not loaded".
        #[cfg(feature = "zaps")]
        zap_signers: Vec<ZapSignerEntry>,
        /// The Dim-3 courtesy read. `None` when the status call itself failed —
        /// the gate then leaves the button live, since the nest, not the app, is
        /// the enforcement floor.
        #[cfg(feature = "zaps")]
        features: Option<Vec<fauna_client_features::FeatureRow>>,
        /// `Some(owed)` only from the nav-enter refresh
        /// ([`refresh_and_check_npub`]) — `None` from every mutation-triggered
        /// refresh, meaning "leave `NostrState::npub_confirmation_owed`
        /// unchanged" (module docs on [`Op::Refresh`]).
        npub_confirmation_owed: Option<bool>,
    },
    /// An invite was minted: the one-time connect string, plus the refreshed
    /// roster it created a pending row in. Carried as its own variant because
    /// the string must survive exactly one fold — a plain `Loaded` would clear
    /// it (`nostr.md:52` — shown once).
    InviteMinted {
        connect_string: String,
        bunker_apps: Vec<BunkerAppEntry>,
    },
    /// A transport or provider failure — lands on `error-message`.
    Failed(String),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh { nest, account } => refresh_and_check_npub(nest, account).await,
            Op::Link {
                nest,
                account,
                mode,
                params,
            } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.link(BRIDGE_ID, mode, params).await {
                    Ok(_) => {
                        // Best-effort, and deliberately swallowed: a failed
                        // write here just means the next real check re-asks,
                        // the harmless direction every leg of this plane takes
                        // (module docs on `Op::Refresh`).
                        if let Some(account) = &account {
                            let _ = account.confirm_nostr_npub(now_secs()).await;
                        }
                        refresh_and_check_npub(nest, account).await
                    }
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::Unlink { nest } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.unlink(BRIDGE_ID).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::SetSettings { nest, settings } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.set_settings(BRIDGE_ID, settings).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::AddFollow { nest, id, petname } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.add_follow(BRIDGE_ID, id, petname, None).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::RemoveFollow { nest, id } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.remove_follow(BRIDGE_ID, id).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::CreateInvite { nest } => {
                let client = NostrBunkerClient::new(Arc::clone(&nest));
                let reply = match client.create_invite().await {
                    Ok(r) => r,
                    Err(e) => return Outcome::Failed(e.to_string()),
                };
                // Re-list rather than synthesizing the pending row locally: the
                // roster is server-authoritative (it carries created/expiry the
                // client never computes), the same posture follows take.
                match client.list().await {
                    Ok(apps) => Outcome::InviteMinted {
                        connect_string: reply.connect_string,
                        bunker_apps: apps,
                    },
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            #[cfg(feature = "zaps")]
            Op::AddZapSigner {
                nest,
                pubkey,
                label,
            } => {
                let client = NostrZapSignerClient::new(Arc::clone(&nest));
                // The reply carries the STORED row (lowercased), but we re-list
                // rather than push it: the roster is server-authoritative, the
                // same non-optimistic posture every other mutation here takes —
                // and it is what makes the rendered row the nest's row.
                match client.add(pubkey, label).await {
                    Ok(_) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            #[cfg(feature = "zaps")]
            Op::RemoveZapSigner { nest, pubkey } => {
                let client = NostrZapSignerClient::new(Arc::clone(&nest));
                match client.remove(pubkey).await {
                    Ok(_) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::ConfirmNpub { nest, account } => {
                match account.confirm_nostr_npub(now_secs()).await {
                    // Re-checks rather than assuming `Some(false)`: the fresh
                    // read is what makes this non-optimistic like every other
                    // mutation on this page.
                    Ok(_) => refresh_and_check_npub(nest, Some(account)).await,
                    Err(e) => Outcome::Failed(format!("{e:#}")),
                }
            }
        }
    }
}

/// The caller's own clock, in epoch seconds — the unit the
/// `fauna.state.nostr-confirmation` stamp is stored in.
fn now_secs() -> i64 {
    fauna_core::data::Timestamp::now_secs_or_zero()
}

async fn refresh(nest: Arc<NestClient>) -> Outcome {
    let client = BridgesClient::new(Arc::clone(&nest));
    let bridges = match client.list().await {
        Ok(r) => r.bridges,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let status = bridges.into_iter().find(|b| b.id == BRIDGE_ID);
    // `registered` is exactly "the bridge is in the list" — see the module
    // docs on why this, and never `available`, gates the unavailable notice.
    let registered = status.is_some();
    // Follows only mean anything for a registered bridge, and `list_follows`
    // on an unregistered one is a guaranteed rejection — skip it rather than
    // turn a legitimate "no nostr feature" into a page error.
    let follows = if registered {
        match client.list_follows(BRIDGE_ID).await {
            Ok(r) => r.follows,
            Err(e) => return Outcome::Failed(e.to_string()),
        }
    } else {
        Vec::new()
    };
    // The roster exists only for a linked CUSTODIAL account — a `remote`/`nip07`
    // account has no key on the box to sign with, so the nest offers it no
    // bunker role (`nostr.md:50`). Asking anyway would turn a correct refusal
    // into a page error.
    let custodial_linked = status.as_ref().is_some_and(|s| {
        s.linked && matches!(s.mode.as_deref(), Some("generated") | Some("imported"))
    });
    let bunker_apps = if custodial_linked {
        match NostrBunkerClient::new(Arc::clone(&nest)).list().await {
            Ok(apps) => apps,
            Err(e) => return Outcome::Failed(e.to_string()),
        }
    } else {
        Vec::new()
    };
    // The zap-signer trust root is keyed by the ACTOR, and a receipt is believed
    // only when its `p` tag names this payee — so the roster means something
    // exactly when an account is linked, custodial or not: unlike the bunker
    // role (which needs a key on the box to sign with), designating who may
    // speak for your money is orthogonal to where your key lives.
    #[cfg(feature = "zaps")]
    let (zap_signers, features) = if status.as_ref().is_some_and(|s| s.linked) {
        let signers = match NostrZapSignerClient::new(Arc::clone(&nest)).list().await {
            Ok(rows) => rows,
            Err(e) => return Outcome::Failed(e.to_string()),
        };
        // The courtesy read is best-effort and deliberately swallowed: the nest
        // is the enforcement floor, so a failed status read must not turn a
        // working page into an error — it just leaves the button live and lets
        // the real refusal speak for itself.
        let rows = fauna_client_features::FeaturesClient::new(Arc::clone(&nest))
            .rows()
            .await
            .ok();
        (signers, rows)
    } else {
        (Vec::new(), None)
    };
    Outcome::Loaded {
        registered,
        status: status.map(Box::new),
        follows,
        bunker_apps,
        #[cfg(feature = "zaps")]
        zap_signers,
        #[cfg(feature = "zaps")]
        features,
        // Never touched by the plain refresh — see the field doc.
        npub_confirmation_owed: None,
    }
}

/// [`refresh`], plus the succession-aftermath npub check — the nav-enter
/// path's extra leg (module docs on [`Op::Refresh`]).
async fn refresh_and_check_npub(
    nest: Arc<NestClient>,
    account: Option<AccountStoreHandle>,
) -> Outcome {
    // No account runtime yet degrades to "not owed" inside the shared read,
    // the same direction it takes on any other failure.
    let owed = fauna_client_config::npub_confirmation_owed_for(
        nest.as_ref(),
        account.as_ref().map(|a| async move {
            let read = a.npub_confirmed_at().await;
            if let Err(e) = &read {
                tracing::warn!("nostr: npub-confirm stamp unreadable — not owed: {e:#}");
            }
            read
        }),
    )
    .await;
    let mut outcome = refresh(nest).await;
    if let Outcome::Loaded {
        npub_confirmation_owed,
        ..
    } = &mut outcome
    {
        *npub_confirmation_owed = Some(owed);
    }
    outcome
}

/// Fold an op's result back into the page. One function for both dispatch
/// paths, so they cannot disagree about what an outcome means.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    let st = &mut app.nostr;
    match outcome {
        Outcome::Loaded {
            registered,
            status,
            follows,
            bunker_apps,
            #[cfg(feature = "zaps")]
            zap_signers,
            #[cfg(feature = "zaps")]
            features,
            npub_confirmation_owed,
        } => {
            let was_linked = st.linked();
            st.registered = registered;
            st.status = status.map(|b| *b);
            st.follows = follows;
            st.bunker_apps = bunker_apps;
            #[cfg(feature = "zaps")]
            {
                st.zap_signers = zap_signers;
                st.features = features;
            }
            // Any ordinary load ends the one-time reveal — the secret it
            // carries is single-use, so leaving it painted would advertise a
            // credential that no longer works (`nostr.md:52`).
            st.bunker_connect_string = None;
            // A freshly linked account must not leave the pasted key sitting in
            // a buffer (`nostr.md` § Persistence — a client holds nsec material
            // only transiently, for the request that consumes it).
            if !was_linked && st.linked() {
                st.nsec_input.clear();
            }
            // `None` means an ordinary mutation-triggered plain `refresh` ran,
            // which never re-checks — leave whatever the last nav-enter,
            // confirm, or (successful) link check said. `Op::Link` always
            // confirms before this refresh runs, so a fresh link's `Some`
            // reads back `false` for real rather than by assumption.
            if let Some(owed) = npub_confirmation_owed {
                st.npub_confirmation_owed = owed;
            }
            app.errors.remove(&Page::Nostr);
        }
        Outcome::InviteMinted {
            connect_string,
            bunker_apps,
        } => {
            st.bunker_connect_string = Some(connect_string);
            st.bunker_apps = bunker_apps;
            app.errors.remove(&Page::Nostr);
        }
        Outcome::Failed(e) => {
            app.errors.insert(Page::Nostr, e);
        }
    }
}

// ── Elements ─────────────────────────────────────────────────────────────────

/// The Nostr page as one ordered element list (paint = registry = focus ring).
///
/// The **link gate** is the load-bearing structure here: unlinked paints only
/// the account-link form, linked paints the account + settings + relays +
/// follows. Rendering both unconditionally is precisely the bug linux shipped
/// (`nostr.md:201`) — `nostr-pubkey-copy-btn` is the cross-app `is_linked()`
/// signal, so painting it while unlinked makes every downstream test read a
/// stale link state and silently no-op.
pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.nostr;
    let mut out = vec![Element::label(ids::PAGE_HEADING, t::TITLE)];

    // A nest built without the `nostr` cargo feature: the bridge is absent from
    // `fauna.bridges.list` entirely and nothing on this page can work. This is
    // the ONLY gate the unavailable notice hangs on.
    if !st.registered {
        // Untagged chrome: ui.yaml gives the notice no element ID, and
        // registering one would be the invented-ID anti-pattern (the Media
        // empty-state precedent). Nothing actionable renders, which is itself
        // the observable — `is_page_visible()` keys on the link/account
        // controls, and neither exists without a bridge.
        out.push(Element::chrome(t::UNAVAILABLE));
        return out;
    }

    if st.linked() {
        linked_elements(st, &mut out);
    } else {
        link_form_elements(st, &mut out);
    }
    out
}

/// The unlinked surface: mode picker, the mode's credential field, and submit.
fn link_form_elements(st: &NostrState, out: &mut Vec<Element>) {
    out.push(Element::chrome(t::link_account::TITLE));
    out.push(
        Element::select(
            ids::NOSTR_LINK_MODE,
            st.effective_link_mode().to_string(),
            SelectTarget::NostrLinkMode,
            LINK_MODES.iter().map(|m| m.to_string()).collect(),
        )
        .labelled(t::link_account::MODE_LABEL)
        // `options`/`text` stay the raw wire tokens the automation driver
        // matches (ui-actual-tui.yaml's documented "raw-token picker"
        // contract, unchanged); this is only the human-readable paint of the
        // *current* selection — the shared
        // `fauna_client_bridges::nostr_link_mode_label` map windows/linux
        // also resolve, so the three native apps stop hand-writing their own
        // copy of the same match (`docs/goal/ui/nostr.md` § Account linking).
        .display_value(crate::wizard::localized(&nostr_link_mode_label(
            st.effective_link_mode(),
        ))),
    );
    // Import mode alone reveals the nsec field — a password-shaped buffer that
    // must not linger on screen for the other two modes.
    if st.effective_link_mode() == MODE_IMPORT {
        out.push(
            Element::input(
                ids::NOSTR_NSEC_INPUT,
                st.nsec_input.clone(),
                Field::Nostr(NostrField::Nsec),
            )
            .labelled(t::link_account::NSEC_PLACEHOLDER),
        );
    }
    // The bunker-URL field carries no ui.yaml id — the native-client
    // convention linux/apple/android share (`nostr.md:93`).
    if st.effective_link_mode() == MODE_REMOTE {
        out.push(
            // An EMPTY id: focusable and paintable, but not automatable —
            // ui.yaml gives this field no ID, and linux/apple/android all carry
            // it untagged for the same reason (`nostr.md:93`).
            Element::input(
                String::new(),
                st.bunker_url_input.clone(),
                Field::Nostr(NostrField::BunkerUrl),
            )
            .labelled(t::account::SIGNING_MODE),
        );
    }
    out.push(Element::gesture_button(
        ids::NOSTR_LINK_BUTTON,
        t::link_account::LINK_BUTTON,
        true,
        Gesture::Nostr(Action::Link),
    ));
}

/// The linked surface: account rows, the 5 content flags, relays, follows.
fn linked_elements(st: &NostrState, out: &mut Vec<Element>) {
    // ── Account ──
    // The npub itself is chrome — ui.yaml scopes only the COPY button here, so
    // the value is painted for the human and copied by the button.
    out.push(Element::chrome(st.npub().unwrap_or("—").to_string()));
    // The e2e `is_linked()` signal — gated on the SAME predicate the rest of
    // this branch is (module docs on `NostrState::linked`).
    out.push(Element::gesture_button(
        ids::NOSTR_PUBKEY_COPY_BTN,
        common::COPY,
        true,
        Gesture::Nostr(Action::CopyPubkey),
    ));
    // The localized stored-mode label from the ONE shared map, so every app
    // renders this row identically (`nostr.md:139`). An unknown/absent value
    // renders verbatim, which is the shared map's own documented behavior.
    if let Some(mode) = st.signing_mode() {
        out.push(Element::chrome(crate::wizard::localized(
            &nostr_key_source_label(mode),
        )));
    }
    out.push(Element::gesture_button(
        ids::NOSTR_UNLINK_BUTTON,
        t::account::UNLINK_BUTTON,
        true,
        Gesture::Nostr(Action::Unlink),
    ));

    // ── Succession-aftermath npub confirm (leg 3 — `nostr.md` § Key
    // succession and rotation) ──
    //
    // Dismissible, never a blocking modal — nostr.md's own Gotcha: the user
    // may reach this page long after the succession, and what is owed is a
    // *deliberate* confirmation surface, not a lock.
    if st.npub_confirmation_owed {
        out.push(Element::label(
            ids::NOSTR_NPUB_CONFIRM_BANNER,
            t::npub_confirm::BANNER.replace("{npub}", st.npub().unwrap_or("—")),
        ));
        out.push(Element::gesture_button(
            ids::NOSTR_NPUB_CONFIRM_YES_BUTTON,
            t::npub_confirm::YES_BUTTON,
            true,
            Gesture::Nostr(Action::ConfirmNpub),
        ));
        out.push(Element::gesture_button(
            ids::NOSTR_NPUB_CONFIRM_NO_BUTTON,
            t::npub_confirm::NO_BUTTON,
            true,
            Gesture::Nostr(Action::DismissNpubToNewKey),
        ));
    }

    // ── Content settings ──
    // The five rows — id, wire key, default AND label — come from the shared
    // catalog every app now renders (`nostr.md` § Where logic lives); tui
    // paints the title only, its checkbox row having no second line.
    for opt in nostr_content_toggle_options() {
        let on = st.toggle(&opt.key, opt.default_on);
        out.push(
            Element::checkbox_gesture(
                opt.ui_id,
                opt.label.resolve(fauna_i18n::strings::lookup),
                on,
                Gesture::Nostr(Action::ToggleContent {
                    key: opt.key,
                    // The gesture carries the value the tap should PRODUCE, so
                    // a click is idempotent from the driver's side: `set_toggle`
                    // only clicks when the read state differs from the target.
                    value: !on,
                }),
            )
            // The cross-app `driver.get_attr(id, "state")` contract — the
            // marker linux's 3 pre-existing toggles had shipped without.
            .attr("state", if on { "on" } else { "off" }),
        );
    }

    // ── Relays ──
    out.push(Element::chrome(t::relays::TITLE));
    let relays = st.relays();
    for (i, url) in relays.iter().enumerate() {
        // Flat indexed ids — the client-wide convention, and what
        // `count("nostr-relay-item")` / `click("nostr-remove-relay", index=i)`
        // read (`nostr.md` § Architectural rules 3).
        out.push(Element::label(ids::NOSTR_RELAY_ITEM, url.clone()));
        out.push(Element::gesture_button(
            ids::NOSTR_REMOVE_RELAY,
            common::REMOVE,
            true,
            Gesture::Nostr(Action::RemoveRelay {
                // The paint index, not a value search: the driver's click passes
                // only an occurrence index, so row `i` must drop entry `i` — and
                // a search by URL would drop the FIRST match, silently removing
                // the wrong row if the list ever held a duplicate.
                index: i,
            }),
        ));
    }
    out.push(
        Element::input(
            ids::NOSTR_RELAY_INPUT,
            st.relay_input.clone(),
            Field::Nostr(NostrField::Relay),
        )
        .labelled(t::relays::PLACEHOLDER),
    );
    out.push(Element::gesture_button(
        ids::NOSTR_ADD_RELAY,
        t::relays::ADD,
        true,
        Gesture::Nostr(Action::AddRelay),
    ));

    // ── Follows ──
    out.push(Element::chrome(t::follows::TITLE));
    for f in &st.follows {
        out.push(Element::label(ids::NOSTR_FOLLOW_ITEM, follow_display(f)));
        out.push(Element::gesture_button(
            ids::NOSTR_REMOVE_FOLLOW,
            common::REMOVE,
            true,
            // Keyed by the follow's own id, not a positional index: the nest's
            // remove takes the id, and a list that shifted under us must still
            // remove the row the user actually saw.
            Gesture::Nostr(Action::RemoveFollow { id: f.id.clone() }),
        ));
    }
    out.push(
        Element::input(
            ids::NOSTR_FOLLOW_PUBKEY_INPUT,
            st.follow_pubkey_input.clone(),
            Field::Nostr(NostrField::FollowPubkey),
        )
        .labelled(t::follows::PUBKEY_PLACEHOLDER),
    );
    out.push(
        Element::input(
            ids::NOSTR_FOLLOW_PETNAME_INPUT,
            st.follow_petname_input.clone(),
            Field::Nostr(NostrField::FollowPetname),
        )
        .labelled(t::follows::PETNAME_PLACEHOLDER),
    );
    out.push(Element::gesture_button(
        ids::NOSTR_ADD_FOLLOW,
        t::follows::ADD,
        true,
        Gesture::Nostr(Action::AddFollow),
    ));

    // ── Connected apps (NIP-46 bunker) ──
    //
    // Custodial-only: a `remote`/`nip07` account keeps its key outside the box,
    // so there is nothing here for the nest to sign with and the section is not
    // offered at all (`nostr.md:50`).
    if st.custodial() {
        connected_apps_elements(st, out);
    }

    // ── Zap signers (the NIP-57 trust root) ──
    //
    // NOT custodial-gated, unlike Connected apps above: the bunker role needs a
    // key on the box to sign WITH, but designating who may speak for your money
    // is orthogonal to where your key lives. Any linked account has a pubkey a
    // receipt's `p` tag can name, so any linked account can be a payee.
    #[cfg(feature = "zaps")]
    zap_signers_elements(st, out);
}

/// The *Zap signers* section (`monetization.md` § Zap receipts — the trust
/// model, ratified 2026-07-29; page slot `nostr.md` § Layout & flow item 7).
///
/// A kind-9735 zap receipt is signed by the *recipient's* wallet server — not
/// the sender's, and not any key Fauna knows a priori — and anyone may mint one
/// naming any recipient. Its own valid signature therefore proves nothing. The
/// trust root is this list, and it is app UI + nest state precisely because
/// "which wallet provider speaks for my money" is a genuine user choice
/// (§ Product invariants — there is no operator to hand-edit a file).
///
/// **The empty roster is a stated state, not a blank list.** A payee who has
/// designated nobody believes nobody — the ratified out-of-the-box default, and
/// the correct one (no zap is silently believed). Rendering it as an empty-list
/// shrug would read as "nothing here yet" when it actually means "every zap you
/// receive is currently inert", so it gets its own element and says so.
#[cfg(feature = "zaps")]
fn zap_signers_elements(st: &NostrState, out: &mut Vec<Element>) {
    out.push(Element::chrome(t::zap_signers::TITLE));
    out.push(Element::chrome(t::zap_signers::DESCRIPTION));

    for signer in &st.zap_signers {
        out.push(Element::label(
            ids::NOSTR_ZAP_SIGNER_ITEM,
            zap_signer_row_text(signer),
        ));
        out.push(Element::gesture_button(
            ids::NOSTR_ZAP_SIGNER_REMOVE,
            t::zap_signers::REMOVE,
            // Always live: removal is de-escalation, which is never gated
            // (`dynamic-features.md`, ruling (i)) — a tier that can only tighten
            // must not be able to trap a user in a roster they cannot undo.
            true,
            Gesture::Nostr(Action::RemoveZapSigner {
                // Keyed by the row's own STORED pubkey, like the bunker roster
                // is keyed by its connection id: a roster that shifted under us
                // must still remove the signer the user actually saw.
                pubkey: signer.signer_pubkey.clone(),
            }),
        ));
    }
    if st.zap_signers.is_empty() {
        out.push(Element::label(
            ids::NOSTR_ZAP_SIGNER_EMPTY,
            t::zap_signers::NONE,
        ));
    }

    out.push(
        Element::input(
            ids::NOSTR_ZAP_SIGNER_PUBKEY_INPUT,
            st.zap_signer_pubkey_input.clone(),
            Field::Nostr(NostrField::ZapSignerPubkey),
        )
        .labelled(t::zap_signers::PUBKEY_PLACEHOLDER),
    );
    out.push(
        Element::input(
            ids::NOSTR_ZAP_SIGNER_LABEL_INPUT,
            st.zap_signer_label_input.clone(),
            Field::Nostr(NostrField::ZapSignerLabel),
        )
        .labelled(t::zap_signers::LABEL_PLACEHOLDER),
    );
    // The Dim-3 courtesy layer (`dynamic-features.md` § Evaluation points item
    // 2). `zaps.signer.designate` is a WIRED gate surface — the nest refuses
    // this call with a typed `fauna.features.denied` / `.over_quota` when the
    // plane says no — so offering a live button guaranteed to fail is exactly
    // the "letting the user hit refusals" the courtesy layer exists to prevent.
    // The `subscription-claim-redeem-button` precedent, one plane over.
    let gate = designate_gate(st);
    out.push(Element::gesture_button(
        ids::NOSTR_ZAP_SIGNER_ADD_BTN,
        t::zap_signers::ADD,
        gate.is_none(),
        Gesture::Nostr(Action::AddZapSigner),
    ));
    // Rule 5 — a disabled control states its reason within eyeshot, and the
    // reason is the honest why-line the shared crate already composed.
    if let Some(reason) = gate {
        out.push(Element::chrome(reason));
    }
}

/// A roster row's text: the user's label (or a placeholder) and the signer's
/// short pubkey. Owned by `fauna_client_nostr` — see its doc comment for the
/// STORED-vs-typed-pubkey rationale.
#[cfg(feature = "zaps")]
pub use fauna_client_nostr::zap_signer_row_text;

/// Why `nostr-zap-signer-add-btn` is dead, or `None` when it works.
///
/// `zaps` is the member that gates designation. Three shapes are load-bearing:
///
/// * **The decision is READ, never re-derived.** `affordance` is the shared
///   crate's and already accounts for the **subset edge** — a `payments` deny
///   reaches `zaps`, because a zap's only consequences are payments-plane
///   surfaces — and for the newness-delta rule. A page composing its own meet
///   would report `zaps` available under a `payments` deny the nest refuses,
///   which is a silent gate wearing the opposite costume.
/// * **A `hidden` affordance still disables rather than vanishing.** `zaps` has
///   no capability token today (absence proves nothing about a plane the nest
///   does not advertise), so this arm is unreachable in practice — but if it is
///   ever reached, a button that silently disappears is the silent gate
///   boundary 4 forbids. The *excision* story is the orthogonal COMPILE-TIME
///   axis: this crate's `zaps` feature makes the whole section absent from the
///   artifact, where this one only greys the button in a build that HAS it.
/// * **An un-hydrated read leaves the button LIVE.** Nothing is painted while
///   the courtesy read is missing: the nest, not the app, is the enforcement
///   floor, and a settled refusal with no basis is worse than a blank.
#[cfg(feature = "zaps")]
fn designate_gate(st: &NostrState) -> Option<String> {
    fauna_client_features::gate_reason(
        st.features.as_deref().unwrap_or(&[]),
        "zaps",
        fauna_i18n::strings::lookup,
    )
}

/// The *Connected apps* section (`nostr.md` § The nest as the user's NIP-46
/// signer). Third-party Nostr apps sign via the user's own box; the deposited
/// nsec never leaves it.
///
/// **No QR here, by ratified design.** `ui.yaml` scopes
/// `nostr-bunker-connect-qr` to `optional_elements` precisely so tui can render
/// the string alone (`nostr.md:88` — "QR platform-scoped — tui renders the
/// string only"). A QR exists on the GUI apps because a phone camera scans
/// it off a desktop screen; a terminal user copies the string.
fn connected_apps_elements(st: &NostrState, out: &mut Vec<Element>) {
    out.push(Element::chrome(t::connected_apps::TITLE));
    out.push(Element::gesture_button(
        ids::NOSTR_BUNKER_CONNECT_BTN,
        t::connected_apps::CONNECT_BUTTON,
        true,
        Gesture::Nostr(Action::ConnectApp),
    ));
    // The one-time reveal — present only between the mint and the next load.
    if let Some(connect) = &st.bunker_connect_string {
        out.push(Element::label(
            ids::NOSTR_BUNKER_CONNECT_STRING,
            connect.clone(),
        ));
        out.push(Element::gesture_button(
            ids::NOSTR_BUNKER_CONNECT_COPY_BTN,
            common::COPY,
            true,
            Gesture::Nostr(Action::CopyConnectString),
        ));
    }
    // The connections themselves (`nostr-bunker-app-item` + its revoke) moved
    // to the Settings → Connected apps roster on tui, as signer-client rows —
    // a lift, never a duplication (`ui/connected-apps.md` § Architectural
    // rules). This section keeps the NIP-46 start: minting an invite.
}

// ── e2e state serializer ─────────────────────────────────────────────────────

/// The `data.nostr` half of `GET /app/state` — the bridge's link state plus the
/// two lists, so a driver can assert without walking elements.
pub fn state_json(state: &NostrState) -> Value {
    json!({
        "registered": state.registered,
        "linked": state.linked(),
        "signing_mode": state.signing_mode(),
        "npub": state.npub(),
        "npub_confirmation_owed": state.npub_confirmation_owed,
        "relays": state.relays(),
        "follows": state
            .follows
            .iter()
            .map(|f| json!({ "id": f.id, "petname": f.petname }))
            .collect::<Vec<_>>(),
        "bunker_apps": state
            .bunker_apps
            .iter()
            .map(|a| json!({ "id": a.id, "label": a.label, "status": a.status }))
            .collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::authed_app;
    use fauna_protocol::bridges_ui::BridgeSetting;

    fn setting(key: &str, value: WireValue) -> BridgeSetting {
        BridgeSetting {
            key: key.to_string(),
            label: key.to_string(),
            setting_type: "bool".to_string(),
            value,
            options: None,
            extra: Default::default(),
        }
    }

    fn status(linked: bool, mode: Option<&str>, settings: Vec<BridgeSetting>) -> BridgeStatus {
        BridgeStatus {
            id: BRIDGE_ID.to_string(),
            name: "Nostr".to_string(),
            // Deliberately FALSE in every fixture: `available` is the
            // nsec-deposit bridging gate and is false on a fresh box, so the
            // tests prove the page renders regardless of it.
            available: false,
            linked,
            identity: linked.then(|| fauna_protocol::bridges_ui::BridgeIdentity {
                label: "npub".to_string(),
                value: "npub1testtesttest".to_string(),
                display: "npub1test…".to_string(),
                extra: Default::default(),
            }),
            mode: mode.map(str::to_string),
            settings,
            supports_follows: true,
            supports_follow_requests: false,
            link_modes: None,
            glyph: None,
            error: None,
            extra: Default::default(),
        }
    }

    fn nostr_app() -> App {
        let mut app = authed_app();
        app.page = Page::Nostr;
        app.nostr.link_mode = MODE_GENERATE.to_string();
        app
    }

    fn ids(app: &App) -> Vec<String> {
        elements(app).into_iter().map(|e| e.id).collect()
    }

    fn count_id(app: &App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    fn attr_of(app: &App, id: &str, key: &str) -> Option<String> {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .and_then(|e| {
                e.attrs
                    .iter()
                    .find(|(k, _)| k == key)
                    .map(|(_, v)| v.clone())
            })
    }

    /// **The bug this page exists not to repeat** (`nostr.md:191`): `available`
    /// is the nsec-deposit bridging gate and is `false` on every fresh box, so
    /// gating the link form on it would hide the only control that can deposit
    /// the first nsec — the box could never bootstrap. A registered-but-
    /// unavailable bridge must still paint the full link form.
    #[test]
    fn an_unavailable_but_registered_bridge_still_paints_the_link_form() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        assert!(!app.nostr.status.as_ref().unwrap().available);

        let got = ids(&app);
        for required in ["nostr-link-mode", "nostr-link-button"] {
            assert!(
                got.contains(&required.to_string()),
                "{required} must render on an unavailable box"
            );
        }
        assert!(
            !elements(&app).iter().any(|e| e.text == t::UNAVAILABLE),
            "`available` must gate NOTHING — only `registered` does"
        );
    }

    /// An unregistered bridge (a nest built without the `nostr` cargo feature)
    /// is the one case that paints the notice instead of the page.
    #[test]
    fn an_unregistered_bridge_paints_only_the_unavailable_notice() {
        let app = nostr_app();
        assert!(!app.nostr.registered);
        let got = ids(&app);
        assert!(
            !got.contains(&"nostr-link-button".to_string()),
            "nothing actionable renders when the bridge is absent"
        );
        assert!(
            !got.contains(&"nostr-pubkey-copy-btn".to_string()),
            "nor the linked surface"
        );
        // The notice itself is untagged chrome (no ui.yaml id), so it is
        // painted but never registered — assert on the TEXT, not an id.
        assert!(
            elements(&app).iter().any(|e| e.text == t::UNAVAILABLE),
            "the notice is still painted for the human"
        );
    }

    /// The link gate: unlinked paints the form and NOT the account surface;
    /// linked swaps them. `nostr-pubkey-copy-btn` is the cross-app
    /// `is_linked()` signal, so it must never paint while unlinked — the exact
    /// regression linux shipped (`nostr.md:201`).
    #[test]
    fn the_link_gate_swaps_the_form_for_the_account_surface() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        assert_eq!(count_id(&app, "nostr-pubkey-copy-btn"), 0);
        assert_eq!(count_id(&app, "nostr-link-button"), 1);
        assert_eq!(count_id(&app, "nostr-expose-content"), 0);
        assert_eq!(count_id(&app, "nostr-relay-input"), 0);

        app.nostr.status = Some(status(true, Some("generated"), vec![]));
        assert_eq!(count_id(&app, "nostr-pubkey-copy-btn"), 1);
        assert_eq!(count_id(&app, "nostr-link-button"), 0);
        assert_eq!(count_id(&app, "nostr-unlink-button"), 1);
        // Every one of the 5 content toggles + both list sections.
        for opt in nostr_content_toggle_options() {
            let id = &opt.ui_id;
            assert_eq!(count_id(&app, id), 1, "{id} renders once linked");
        }
        assert_eq!(count_id(&app, "nostr-relay-input"), 1);
        assert_eq!(count_id(&app, "nostr-follow-pubkey-input"), 1);
    }

    /// A never-touched picker must still submit `generate` — the action layer's
    /// `link_generate()` clicks the button without selecting a mode, so an empty
    /// buffer posting an empty `fauna.bridges.link` mode would fail every
    /// link test on every app.
    #[test]
    fn an_untouched_link_mode_defaults_to_generate() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        app.nostr.link_mode.clear();
        assert_eq!(app.nostr.effective_link_mode(), MODE_GENERATE);
        assert_eq!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "nostr-link-mode")
                .expect("the picker renders")
                .text,
            MODE_GENERATE,
            "the picker round-trips the token it would submit"
        );
        // …and no credential field is revealed for it.
        assert_eq!(count_id(&app, "nostr-nsec-input"), 0);
    }

    /// The nsec field is import-mode only — a password-shaped buffer must not
    /// linger on the generate/remote surfaces.
    #[test]
    fn the_nsec_field_renders_only_in_import_mode() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        assert_eq!(count_id(&app, "nostr-nsec-input"), 0);

        apply_local(&mut app, Action::SetLinkMode(MODE_IMPORT.to_string()));
        assert_eq!(count_id(&app, "nostr-nsec-input"), 1);

        apply_local(&mut app, Action::SetLinkMode(MODE_REMOTE.to_string()));
        assert_eq!(count_id(&app, "nostr-nsec-input"), 0);
    }

    /// Import mode with an empty nsec is refused **client-side** and surfaces on
    /// the page error rather than posting an empty credential.
    #[test]
    fn import_mode_with_an_empty_nsec_errors_without_a_nest_call() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        app.nostr.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([3u8; 32]),
        ));
        apply_local(&mut app, Action::SetLinkMode(MODE_IMPORT.to_string()));

        let op = apply_local(&mut app, Action::Link);
        assert!(op.is_none(), "an empty nsec must not reach the nest");
        assert_eq!(
            app.errors.get(&Page::Nostr).map(String::as_str),
            Some(t::link_account::ENTER_NSEC)
        );
    }

    /// The `state` attr is the cross-app toggle contract, and the gesture
    /// carries the value the tap should PRODUCE (so a driver's idempotent
    /// `set_toggle` can compare-then-click).
    #[test]
    fn content_toggles_expose_state_and_carry_the_flipped_value() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(
            true,
            Some("generated"),
            vec![setting("expose_content", WireValue::Bool(true))],
        ));
        assert_eq!(
            attr_of(&app, "nostr-expose-content", "state").as_deref(),
            Some("on")
        );
        // `publish_reactions` defaults false and is absent from the settings.
        assert_eq!(
            attr_of(&app, "nostr-publish-reactions", "state").as_deref(),
            Some("off")
        );
        // …while `publish_replies` defaults TRUE when absent — the nest's own
        // default, so a never-configured account paints what it would do.
        assert_eq!(
            attr_of(&app, "nostr-publish-replies", "state").as_deref(),
            Some("on")
        );
    }

    /// A non-`wss`/`ws` URL is refused by the SHARED predicate, adds no row, and
    /// surfaces an error — the negative case `test_relay_invalid_url_rejected`
    /// drives.
    #[test]
    fn an_invalid_relay_url_is_refused_client_side_with_no_nest_call() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(true, Some("generated"), vec![]));
        app.nostr.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([4u8; 32]),
        ));
        app.nostr.relay_input = "http://not-a-relay.example.com".to_string();

        let op = apply_local(&mut app, Action::AddRelay);
        assert!(op.is_none(), "an invalid URL must not reach the nest");
        assert_eq!(
            app.errors.get(&Page::Nostr).map(String::as_str),
            Some(t::relays::INVALID_URL)
        );
        assert_eq!(count_id(&app, "nostr-relay-item"), 0);

        // A private-network relay is refused by the same predicate with its
        // own message (`network-exposure.md` § Rulings F7).
        app.nostr.relay_input = "ws://192.168.1.10:7777".to_string();
        assert!(apply_local(&mut app, Action::AddRelay).is_none());
        assert_eq!(
            app.errors.get(&Page::Nostr).map(String::as_str),
            Some(t::relays::PRIVATE_ADDRESS)
        );
        assert_eq!(count_id(&app, "nostr-relay-item"), 0);

        // A valid one goes through and clears the buffer.
        app.nostr.relay_input = "wss://relay.example.com".to_string();
        assert!(apply_local(&mut app, Action::AddRelay).is_some());
        assert!(app.nostr.relay_input.is_empty());
        assert!(!app.errors.contains_key(&Page::Nostr));
    }

    /// Relay rows are flat-indexed and each remove carries its own index, so
    /// `click("nostr-remove-relay", index=i)` drops row `i`.
    #[test]
    fn relay_rows_render_indexed_with_their_own_remove() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(
            true,
            Some("generated"),
            vec![setting(
                RELAY_LIST_KEY,
                WireValue::String(r#"["wss://a.example","wss://b.example"]"#.to_string()),
            )],
        ));
        assert_eq!(count_id(&app, "nostr-relay-item"), 2);
        assert_eq!(count_id(&app, "nostr-remove-relay"), 2);
        assert_eq!(
            elements(&app)
                .into_iter()
                .filter(|e| e.id == "nostr-relay-item")
                .map(|e| e.text)
                .collect::<Vec<_>>(),
            vec!["wss://a.example".to_string(), "wss://b.example".to_string()]
        );
    }

    /// A successful link clears the pasted nsec buffer — the key never outlives
    /// the request that consumed it (`nostr.md` § Persistence).
    #[test]
    fn a_successful_link_clears_the_nsec_buffer() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        app.nostr.nsec_input = "nsec1secretsecret".to_string();

        apply_outcome(
            &mut app,
            Outcome::Loaded {
                registered: true,
                status: Some(Box::new(status(true, Some("imported"), vec![]))),
                follows: vec![],
                bunker_apps: vec![],
                #[cfg(feature = "zaps")]
                zap_signers: vec![],
                #[cfg(feature = "zaps")]
                features: None,
                npub_confirmation_owed: None,
            },
        );
        assert!(
            app.nostr.nsec_input.is_empty(),
            "the pasted key must not survive the link that consumed it"
        );
        assert!(app.nostr.linked());
    }

    /// A failed op surfaces on the page error; the next success clears it.
    #[test]
    fn outcomes_fold_onto_the_page_error() {
        let mut app = nostr_app();
        apply_outcome(&mut app, Outcome::Failed("provider exploded".to_string()));
        assert_eq!(
            app.errors.get(&Page::Nostr).map(String::as_str),
            Some("provider exploded")
        );
        apply_outcome(
            &mut app,
            Outcome::Loaded {
                registered: true,
                status: Some(Box::new(status(false, None, vec![]))),
                follows: vec![],
                bunker_apps: vec![],
                #[cfg(feature = "zaps")]
                zap_signers: vec![],
                #[cfg(feature = "zaps")]
                features: None,
                npub_confirmation_owed: None,
            },
        );
        assert!(!app.errors.contains_key(&Page::Nostr));
    }

    fn bunker_app(id: i64, label: &str, status: &str) -> BunkerAppEntry {
        BunkerAppEntry {
            id,
            app_pubkey: None,
            label: label.to_string(),
            status: status.to_string(),
            created_at: 1_700_000_000,
            last_used_at: None,
            use_count: 0,
            expires_at: 1_784_678_400,
            extra: Default::default(),
        }
    }

    /// The *Connected apps* section is custodial-only, and it renders inside the
    /// linked surface — so an unlinked or `remote` account paints no mint button
    /// at all (`nostr.md:50` — a remote account has no key on the box to sign
    /// with, so the nest never offers it the bunker role).
    #[test]
    fn the_connected_apps_section_is_gated_on_a_linked_custodial_account() {
        let mut app = nostr_app();
        app.nostr.registered = true;

        app.nostr.status = Some(status(false, None, vec![]));
        assert_eq!(count_id(&app, "nostr-bunker-connect-btn"), 0, "unlinked");

        app.nostr.status = Some(status(true, Some("remote"), vec![]));
        assert_eq!(count_id(&app, "nostr-bunker-connect-btn"), 0, "remote");

        app.nostr.status = Some(status(true, Some("generated"), vec![]));
        assert_eq!(count_id(&app, "nostr-bunker-connect-btn"), 1, "custodial");
    }

    /// **The ratified tui carve-out** (`ui.yaml` `nostr.optional_elements`,
    /// `nostr.md:88`): the connect string renders, the QR does not. A terminal
    /// user copies the string; the QR exists on GUI apps so a phone camera
    /// can scan it off a screen.
    #[test]
    fn the_connect_reveal_paints_the_string_and_never_a_qr() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(true, Some("generated"), vec![]));

        // Nothing revealed before a mint.
        assert_eq!(count_id(&app, "nostr-bunker-connect-string"), 0);

        apply_outcome(
            &mut app,
            Outcome::InviteMinted {
                connect_string: "bunker://abc?relay=wss://n.test/nostr&secret=s".to_string(),
                bunker_apps: vec![bunker_app(1, "", "pending")],
            },
        );
        assert_eq!(count_id(&app, "nostr-bunker-connect-string"), 1);
        assert_eq!(count_id(&app, "nostr-bunker-connect-copy-btn"), 1);
        assert_eq!(
            count_id(&app, "nostr-bunker-connect-qr"),
            0,
            "tui renders the string only — the QR is ui.yaml-optional for exactly this reason"
        );
    }

    /// The reveal is **one-time**: the secret it carries is single-use, so the
    /// next ordinary load must stop painting it rather than advertise a
    /// credential that no longer works (`nostr.md:52`).
    #[test]
    fn the_one_time_connect_string_clears_on_the_next_load() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(true, Some("generated"), vec![]));
        apply_outcome(
            &mut app,
            Outcome::InviteMinted {
                connect_string: "bunker://one-time".to_string(),
                bunker_apps: vec![bunker_app(1, "", "pending")],
            },
        );
        assert_eq!(count_id(&app, "nostr-bunker-connect-string"), 1);

        apply_outcome(
            &mut app,
            Outcome::Loaded {
                registered: true,
                status: Some(Box::new(status(true, Some("generated"), vec![]))),
                follows: vec![],
                bunker_apps: vec![bunker_app(1, "", "active")],
                #[cfg(feature = "zaps")]
                zap_signers: vec![],
                #[cfg(feature = "zaps")]
                features: None,
                npub_confirmation_owed: None,
            },
        );
        assert_eq!(
            count_id(&app, "nostr-bunker-connect-string"),
            0,
            "a single-use secret must not survive the load after it was shown"
        );
    }

    /// **The "no invisible shim elements" rule, pinned** (the
    /// `wizard_pages_register_only_ui_yaml_ids` precedent): every id this page
    /// REGISTERS must be one ui.yaml scopes to the `nostr` page. Real UI ui.yaml
    /// gives no id — the section headings, the npub value, the signing-mode row,
    /// the native-only bunker-URL field — is painted as untagged chrome (an
    /// empty id, skipped by `ui::register_frame`), never as an invented id.
    #[test]
    fn the_page_registers_only_ui_yaml_scoped_ids() {
        // Exactly `ui.yaml`'s `nostr` page scope (elements + optional_elements),
        // minus the one this client declares absent: `nostr-bunker-connect-qr`
        // (optional — tui renders the string only).
        const UI_YAML_NOSTR_SCOPE: &[&str] = &[
            "page-heading",
            "nostr-pubkey-copy-btn",
            "nostr-link-mode",
            "nostr-nsec-input",
            "nostr-link-button",
            "nostr-unlink-button",
            "nostr-expose-content",
            "nostr-auto-publish",
            "nostr-publish-replies",
            "nostr-publish-reactions",
            "nostr-inbound-to-feed",
            "nostr-relay-item",
            "nostr-relay-input",
            "nostr-add-relay",
            "nostr-remove-relay",
            "nostr-follow-item",
            "nostr-follow-pubkey-input",
            "nostr-follow-petname-input",
            "nostr-add-follow",
            "nostr-remove-follow",
            "nostr-bunker-connect-btn",
            "nostr-bunker-connect-string",
            "nostr-bunker-connect-copy-btn",
            "nostr-zap-signer-item",
            "nostr-zap-signer-pubkey-input",
            "nostr-zap-signer-label-input",
            "nostr-zap-signer-add-btn",
            "nostr-zap-signer-remove",
            "nostr-zap-signer-empty",
            "error-message",
        ];

        let mut app = nostr_app();
        app.nostr.registered = true;

        // Drive every branch that can paint: unregistered, unlinked in each of
        // the three link modes, and the maximal linked+custodial surface with
        // both lists populated and the one-time reveal showing.
        let mut seen: Vec<String> = Vec::new();
        let collect = |app: &App, seen: &mut Vec<String>| {
            seen.extend(
                elements(app)
                    .into_iter()
                    .map(|e| e.id)
                    .filter(|i| !i.is_empty()),
            );
        };

        app.nostr.registered = false;
        collect(&app, &mut seen);

        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        for mode in LINK_MODES {
            apply_local(&mut app, Action::SetLinkMode(mode.to_string()));
            collect(&app, &mut seen);
        }

        app.nostr.status = Some(status(
            true,
            Some("generated"),
            vec![setting(
                RELAY_LIST_KEY,
                WireValue::String(r#"["wss://a.example"]"#.to_string()),
            )],
        ));
        app.nostr.follows = vec![BridgeFollow {
            id: "npub1abc".to_string(),
            petname: Some("alice".to_string()),
            created_at: None,
            extra: None,
            unknown_keys: Default::default(),
        }];
        app.nostr.bunker_apps = vec![bunker_app(1, "Damus", "active")];
        app.nostr.bunker_connect_string = Some("bunker://x".to_string());
        collect(&app, &mut seen);

        seen.sort();
        seen.dedup();
        for id in &seen {
            assert!(
                UI_YAML_NOSTR_SCOPE.contains(&id.as_str()),
                "{id} is not in ui.yaml's `nostr` page scope — paint it as \
                 untagged chrome (Element::chrome) or add it to ui.yaml with approval"
            );
        }
        // Non-vacuous: the maximal surface really did register the whole family.
        for required in [
            "nostr-relay-item",
            "nostr-follow-item",
            "nostr-bunker-connect-string",
        ] {
            assert!(seen.contains(&required.to_string()), "expected {required}");
        }
    }

    /// The custodial dimension the NIP-46 *Connected apps* section is gated on:
    /// a `remote` (bunker) account has no key on the box to sign with.
    #[test]
    fn custodial_is_true_only_for_the_two_key_holding_modes() {
        let mut app = nostr_app();
        for (mode, want) in [
            ("generated", true),
            ("imported", true),
            ("remote", false),
            ("nip07", false),
        ] {
            app.nostr.status = Some(status(true, Some(mode), vec![]));
            assert_eq!(app.nostr.custodial(), want, "custodial({mode})");
        }
    }

    // ── Zap signers (the NIP-57 trust root) ──────────────────────────────────

    #[cfg(feature = "zaps")]
    fn zap_entry(pubkey: &str, label: &str) -> ZapSignerEntry {
        ZapSignerEntry {
            id: 1,
            signer_pubkey: pubkey.to_string(),
            label: label.to_string(),
            created_at: 0,
            extra: Default::default(),
        }
    }

    // `zaps_row` lives in `fauna_client_features::test_fixtures` — one
    // canonical body shared with fauna-linux's identical fixture
    // (`settings/nostr_tab.rs`).
    #[cfg(feature = "zaps")]
    use fauna_client_features::test_fixtures::zaps_row;

    #[cfg(feature = "zaps")]
    fn denied_policy() -> fauna_core::feature_gate::FeaturePolicy {
        fauna_core::feature_gate::FeaturePolicy {
            availability: fauna_core::feature_gate::Availability::Deny,
            ..Default::default()
        }
    }

    #[cfg(feature = "zaps")]
    fn linked_zap_app(mode: &str) -> App {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(true, Some(mode), vec![]));
        // A handle every gesture that reaches the wire needs. Never dialled:
        // `apply_local` only clones it into the op it returns.
        app.nostr.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([3u8; 32]),
        ));
        app
    }

    /// The section is gated on LINKED, not on custodial — unlike Connected
    /// apps. The bunker role needs a key on the box to sign *with*; designating
    /// who may speak for your money is orthogonal to where your key lives, so a
    /// `remote` account is still a payee whose `p` tag a receipt can name.
    #[cfg(feature = "zaps")]
    #[test]
    fn the_section_renders_for_any_linked_account_including_a_remote_one() {
        let mut app = nostr_app();
        app.nostr.registered = true;
        app.nostr.status = Some(status(false, None, vec![]));
        assert!(
            !ids(&app).contains(&ids::NOSTR_ZAP_SIGNER_ADD_BTN.to_string()),
            "an unlinked account has no pubkey to be a payee — nothing to designate for"
        );

        // A REMOTE account: no key on the box, so Connected apps is absent —
        // but the zap trust root is still meaningful.
        app.nostr.status = Some(status(true, Some("remote"), vec![]));
        let painted = ids(&app);
        assert!(
            !painted.contains(&ids::NOSTR_BUNKER_CONNECT_BTN.to_string()),
            "Connected apps stays custodial-gated"
        );
        assert!(
            painted.contains(&ids::NOSTR_ZAP_SIGNER_ADD_BTN.to_string()),
            "the zap trust root must NOT inherit the custodial gate"
        );
    }

    /// An empty roster is a *stated* state, not a blank list: a payee who has
    /// designated nobody believes nobody, so "nothing here yet" would read as
    /// reassurance where the truth is "every zap you receive is inert".
    #[cfg(feature = "zaps")]
    #[test]
    fn an_empty_roster_states_that_nothing_is_believed() {
        let mut app = linked_zap_app("generated");
        assert_eq!(
            count_id(&app, ids::NOSTR_ZAP_SIGNER_EMPTY),
            1,
            "an empty roster must say so"
        );

        app.nostr.zap_signers = vec![zap_entry(&"ab".repeat(32), "Alby")];
        assert_eq!(
            count_id(&app, ids::NOSTR_ZAP_SIGNER_EMPTY),
            0,
            "the 'nothing is believed' line must not survive a designation"
        );
        assert_eq!(count_id(&app, ids::NOSTR_ZAP_SIGNER_ITEM), 1);
        assert_eq!(count_id(&app, ids::NOSTR_ZAP_SIGNER_REMOVE), 1);
    }

    /// The row renders the STORED pubkey. The nest lowercases on write and only
    /// that form ever matches a receipt, so a roster echoing the user's
    /// uppercase paste would look correct and believe nothing.
    #[cfg(feature = "zaps")]
    #[test]
    fn a_row_renders_the_stored_pubkey_elided_by_the_shared_short_id() {
        let stored = "ab".repeat(32);
        let text = zap_signer_row_text(&zap_entry(&stored, "Alby"));
        assert!(text.starts_with("Alby "), "the label leads: {text:?}");
        assert!(
            text.contains(&fauna_core::format::short_id(&stored)),
            "the row must carry the SHARED short_id of the stored key: {text:?}"
        );
        assert!(
            !text.contains(&stored.to_uppercase()),
            "an uppercase form must never reach the row: {text:?}"
        );
        // An unlabeled row borrows a placeholder rather than rendering blank.
        let unlabeled = zap_signer_row_text(&zap_entry(&stored, "  "));
        assert!(
            unlabeled.starts_with(t::zap_signers::UNNAMED),
            "an unlabeled signer gets the placeholder: {unlabeled:?}"
        );
    }

    /// A non-64-hex key never reaches the wire: the nest refuses it anyway, so
    /// this only spares a guaranteed round trip and names the rule.
    #[cfg(feature = "zaps")]
    #[test]
    fn a_malformed_pubkey_is_refused_locally_and_never_becomes_an_op() {
        let mut app = linked_zap_app("generated");
        app.nostr.zap_signer_pubkey_input = "not-hex".to_string();
        assert!(apply_local(&mut app, Action::AddZapSigner).is_none());
        assert!(app.errors.contains_key(&Page::Nostr));
        // The buffer survives a refusal — the user must be able to fix a typo.
        assert_eq!(app.nostr.zap_signer_pubkey_input, "not-hex");

        // And the verdict does NOT depend on holding a nest handle: it is about
        // the input, so ordering it after the handle would let a page with no
        // session silently swallow the typo instead of naming it.
        let mut app = linked_zap_app("generated");
        app.nostr.nest = None;
        app.nostr.zap_signer_pubkey_input = "abc".to_string();
        assert!(apply_local(&mut app, Action::AddZapSigner).is_none());
        assert!(
            app.errors.contains_key(&Page::Nostr),
            "a malformed key must still be named without a nest handle"
        );
    }

    /// Re-designating an already-designated key is NOT refused: the nest's
    /// `add` is idempotent and refreshes the label, so this is the only rename
    /// path any app offers (no `set_label` affordance exists). Blocking it —
    /// the shape the relay list uses — would deny a rename to spare a round
    /// trip, and unlike a relay re-add it is not a silent no-op: the roster row
    /// visibly changes.
    #[cfg(feature = "zaps")]
    #[test]
    fn a_re_designation_is_allowed_because_it_is_the_rename_path() {
        let mut app = linked_zap_app("generated");
        app.nostr.zap_signers = vec![zap_entry(&"ab".repeat(32), "Alby")];
        app.nostr.zap_signer_pubkey_input = "ab".repeat(32);
        app.nostr.zap_signer_label_input = "My node".to_string();
        let op = apply_local(&mut app, Action::AddZapSigner)
            .expect("a re-designation must reach the nest as a label refresh");
        match op {
            Op::AddZapSigner { label, .. } => assert_eq!(label, "My node"),
            _ => panic!("expected an AddZapSigner op"),
        }
        assert!(!app.errors.contains_key(&Page::Nostr));
    }

    /// A well-formed designation clears both buffers and becomes an op — the
    /// pubkey travelling AS TYPED, because the nest is what normalizes it.
    #[cfg(feature = "zaps")]
    #[test]
    fn a_well_formed_designation_sends_the_key_as_typed_and_clears_the_buffers() {
        let mut app = linked_zap_app("generated");
        app.nostr.zap_signer_pubkey_input = "AB".repeat(32);
        app.nostr.zap_signer_label_input = "  Alby  ".to_string();
        let op = apply_local(&mut app, Action::AddZapSigner).expect("a valid key becomes an op");
        match op {
            Op::AddZapSigner { pubkey, label, .. } => {
                assert_eq!(pubkey, "AB".repeat(32), "the nest normalizes, not the app");
                assert_eq!(label, "Alby", "the label is trimmed");
            }
            _ => panic!("expected an AddZapSigner op"),
        }
        assert!(app.nostr.zap_signer_pubkey_input.is_empty());
        assert!(app.nostr.zap_signer_label_input.is_empty());
    }

    /// The Dim-3 courtesy layer. `zaps.signer.designate` is a WIRED gate
    /// surface, so a live button under a deny is a guaranteed refusal — and it
    /// must say WHY: a control that is merely dead is the silent gate
    /// boundary 4 forbids.
    #[cfg(feature = "zaps")]
    #[test]
    fn a_denied_zaps_plane_disables_designate_and_states_the_reason() {
        let mut app = linked_zap_app("generated");
        app.nostr.features = Some(vec![zaps_row(&[(
            fauna_client_features::RuleTier::Admin,
            denied_policy(),
        )])]);
        app.nostr.zap_signers = vec![zap_entry(&"ab".repeat(32), "Alby")];
        let els = elements(&app);
        let add = els
            .iter()
            .find(|e| e.id == ids::NOSTR_ZAP_SIGNER_ADD_BTN)
            .expect("the designate button always registers");
        assert!(!add.enabled, "a guaranteed refusal must not look live");
        assert!(
            els.iter()
                .any(|e| e.text == "Turned off by your nest admin."),
            "rule 5: the disabled control states its reason within eyeshot"
        );
        // Removal is de-escalation and is never gated: a tier that can only
        // tighten must not trap a user in a roster they cannot undo.
        let remove = els
            .iter()
            .find(|e| e.id == ids::NOSTR_ZAP_SIGNER_REMOVE)
            .expect("the remove button registers for a roster row");
        assert!(
            remove.enabled,
            "removal must stay live under a deny — de-escalation is never gated"
        );
    }

    /// An un-hydrated courtesy read leaves the button LIVE: the nest, not the
    /// app, is the enforcement floor, and a settled refusal with no basis is
    /// worse than a blank. Same for a merely BOUNDED plane.
    #[cfg(feature = "zaps")]
    #[test]
    fn an_unhydrated_or_merely_bounded_read_leaves_designate_live() {
        let mut app = linked_zap_app("generated");
        app.nostr.features = None;
        let els = elements(&app);
        let add = els
            .iter()
            .find(|e| e.id == ids::NOSTR_ZAP_SIGNER_ADD_BTN)
            .expect("the designate button always registers");
        assert!(add.enabled, "an un-hydrated read must not settle a refusal");

        app.nostr.features = Some(vec![zaps_row(&[])]);
        let els = elements(&app);
        let add = els
            .iter()
            .find(|e| e.id == ids::NOSTR_ZAP_SIGNER_ADD_BTN)
            .unwrap();
        assert!(
            add.enabled,
            "a bounded-but-unspent plane must stay actionable"
        );
    }

    /// The offline gate's input — a new gesture must answer the wire question.
    #[cfg(feature = "zaps")]
    #[test]
    fn both_zap_gestures_declare_their_wire_kind() {
        assert_eq!(
            Action::AddZapSigner.wire_kind(),
            Some("fauna.nostr.zap_signers.add")
        );
        assert_eq!(
            Action::RemoveZapSigner {
                pubkey: String::new()
            }
            .wire_kind(),
            Some("fauna.nostr.zap_signers.remove")
        );
    }
}
