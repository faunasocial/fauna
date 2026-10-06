//! The unified feed-side **Bridges** page (`behavior/bridges.md`).
//!
//! **The multi-bridge twin of [`crate::nostr`].** Nostr and Bluesky each own a
//! *dedicated* page (`ui/nostr.md` / `ui/atproto.md`) because they are deep
//! integrations; this page is for the *other* feed-side bridge types — today
//! ActivityPub, tomorrow any new feed protocol — rendered from server-declared
//! metadata, one card per bridge, served identically to all 7 apps from the
//! shared `libs/fauna-client-bridges` (`bridges.md` § Layout & flow). So where
//! `nostr.rs` keys every call to `bridge_id:"nostr"`, this module iterates
//! whatever `fauna.bridges.list` returns, minus the two dedicated-page bridges.
//!
//! **The one exclusion rule is shared, never hand-rolled**
//! ([`fauna_client_bridges::is_unified_bridges_page_bridge`]): the list drops
//! `"nostr"` and `"bluesky"` so their cards never double up against their own
//! pages. `list()` itself stays unfiltered (each dedicated page needs its
//! row), and — since `refresh` below stores the whole unfiltered reply into
//! `BridgesState.bridges` — this page applies the exclusion itself, at RENDER
//! time (`elements`), the same shape web/linux/android apply
//! (`bridges.md` § Scope; `ui/atproto.md` § Migration). [`embed_bridge_card`]
//! is the other consumer of that same unfiltered snapshot: the AT Protocol page's
//! Linked-account panel embeds this module's own `bridge-card` rendering for
//! just the `"bluesky"` row (`ui/atproto.md` § Layout & flow) — one
//! `fauna.bridges.list` fetch, shared by reference, so the two pages can never
//! disagree about a bridge's status.
//!
//! **Where the logic lives.** All of it is already shared: the typed
//! `fauna_client_bridges::BridgesClient` over the unified `fauna.bridges.*`
//! wire, reached here through direct Rust with no FFI hop — the same way linux
//! consumes it (priority #2, the second direct-Rust client). This page composes
//! **zero** provider logic of its own: it renders `BridgeStatus` metadata and
//! posts `fauna.bridges.*` mutations.
//!
//! **Inline, not drill-down.** tui renders every bridge's controls at once
//! (the web/apple shape), not linux's list→detail `NavigationSplitView`: a
//! terminal has no split pane to reveal, and the cross-app action layer
//! drives every non-linux app this way already (`actions/bridges.py` —
//! `_open_bridge` is a no-op off linux). Each bridge's member elements register
//! `.within(ids::BRIDGE_CARD, i)` so a scoped `count("bridge-follow-item",
//! scope="bridge-card[0]")` reads exactly that bridge (the A6 nesting lesson —
//! a nested indexed list that skips the containment declaration silently reads
//! empty; `sync-agent.md:222`). The shared `bridge-action-button` stays the
//! same id for Link and Unlink (`bridges.md` § User actions), so — as on every
//! app — there is no client-side "is it linked" signal, and the e2e verifies
//! the outcome against `fauna.bridges.list` (`test_bridges.py`).
//!
//! **Settings rows and the follow-add inputs carry no ui.yaml id** — the
//! `bridge-card` component scopes only `bridge-action-button` / the follows
//! elements, so a metadata-driven setting toggle/select and the id/petname add
//! fields render as **untagged** (empty-id) controls: painted and keyboard-
//! focusable, but not automatable (the `nostr` bunker-URL field precedent).
//! Minting a per-setting id would be the invented-ID anti-pattern (rule A). The
//! mutation path is still unit-tested via [`apply_local`], the same way the
//! Nostr content toggles are.
//!
//! **Settings auto-save (`bridges.md` § Bridge settings).** A toggle and a
//! select are discrete gestures, so each commits its `set_settings` the moment
//! it fires — the tui analogue of the GUI's debounced auto-save, and non-
//! optimistic like the rest of the page: the refresh that follows a mutation is
//! what repaints, so a switch always reflects what the nest persisted.
//!
//! The async split is the page-module contract's (`apps/tui.md` § The
//! page-module contract): [`apply_local`] lands the synchronous half and hands
//! back an [`Op`]; the agent's click path awaits the op and folds its
//! [`Outcome`] before replying, while the keyboard path spawns it. There is no
//! shared *manager* behind this page (`BridgesClient` is a typed transport
//! wrapper, not a snapshot owner), so errors are written straight to
//! `App::errors` like the contacts/nostr pages — no `sync_page_error` bridge is
//! owed.

use fauna_ui_ids as ids;
use std::collections::BTreeMap;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_bridges::{BridgesClient, follow_display, is_unified_bridges_page_bridge};
use fauna_i18n::strings::{bridges as t, common};
use fauna_protocol::Value as WireValue;
use fauna_protocol::bridge_search_policy::SETTING_TYPE_NUMBER;
use fauna_protocol::bridges_ui::{BridgeFollow, BridgeSetting, BridgeStatus};

use crate::app::App;
use crate::element::{Element, Field, Gesture, SelectTarget};
use crate::pages::Page;

// ── State ────────────────────────────────────────────────────────────────────

/// The Bridges page's state, hung off [`App`]. Mirrors the *bridge* statuses
/// (`bridges.md` § State & data shape — the page renders `BridgeStatus`
/// metadata) rather than inventing a page-local model.
#[derive(Default)]
pub struct BridgesState {
    /// The live WS-RPC channel, installed at the post-auth hook. `None` pre-auth
    /// — every reader degrades gracefully.
    pub nest: Option<Arc<NestClient>>,
    /// The app-wide bridge rows from the LAST unfiltered `fauna.bridges.list`
    /// fetch — every provider, including `"nostr"`/`"bluesky"`. The unified
    /// Bridges page's own [`elements`] applies
    /// [`is_unified_bridges_page_bridge`] itself at render time; a dedicated
    /// page ([`embed_bridge_card`]) reads a specific bridge straight out of
    /// this same list, so the two can never disagree about its status.
    pub bridges: Vec<BridgeStatus>,
    /// `fauna.bridges.list_follows` rows per `bridge_id`, fetched only for a
    /// linked follows-capable bridge (an unlinked one has nothing to list, and
    /// asking would turn a correct refusal into a page error).
    pub follows: BTreeMap<String, Vec<BridgeFollow>>,
    /// Per-`(bridge_id, field_key)` link-form input buffers — the metadata-
    /// driven `bridge-link-field-{key}` inputs. Keyed by both because two
    /// unlinked bridges can each declare a field of the same key.
    pub link_fields: BTreeMap<(String, String), String>,
    /// Per-`bridge_id` "ID to follow" add-form buffer.
    pub follow_id_input: BTreeMap<String, String>,
    /// Per-`bridge_id` "Petname (optional)" add-form buffer.
    pub follow_petname_input: BTreeMap<String, String>,
    /// Per-`(bridge_id, setting_key)` buffer for a `number`-typed
    /// [`BridgeSetting`] (the search-policy cap today). Absent means "no local
    /// edit" — [`BridgesState::number_setting_field`] then falls back to the
    /// live nest value, unlike [`Self::link_field`] which has no live value to
    /// fall back to.
    pub number_setting_inputs: BTreeMap<(String, String), String>,
    /// The `(bridge_id, operation, target)` triples this session saw refused by
    /// the guardian gate (`family-safety.md` § Feed-source approvals). Purely
    /// the *local* half of the ask surface: it is what turns a refusal the user
    /// just hit into a visible `bridge-source-request-button`.
    ///
    /// The DURABLE half is `FamilyState::own_feed_requests`, off
    /// `fauna.family.status` — that is what survives navigation and restart, and
    /// what makes the state honest on a fresh session where this page never saw
    /// the refusal. Render reads the durable one first, this second, exactly as
    /// the contacts page orders its two (`contacts.rs`: "the durable list is
    /// what survives navigation and a restart").
    pub guardian_refused: std::collections::BTreeSet<(String, String, String)>,
}

impl BridgesState {
    /// The declared modes that apply on tui — the shared platform filter
    /// (`fauna_client_bridges::applicable_modes`, lifted 2026-08-15). tui had
    /// NO filter before this: a `platform`-scoped mode (Nostr's `"web"`
    /// NIP-07) counted toward `link_block`'s applicable count and could become
    /// [`Self::primary_mode`], rendering a form whose `client_action` (a
    /// browser extension) this app cannot perform — the live-but-inert control
    /// the link-block rule exists to prevent.
    fn platform_modes(bridge: &BridgeStatus) -> Vec<fauna_protocol::bridges_ui::BridgeLinkMode> {
        bridge
            .link_modes
            .as_deref()
            .map(|m| fauna_client_bridges::applicable_modes(m, "tui"))
            .unwrap_or_default()
    }

    /// The link *request* mode for an unlinked bridge — its single applicable
    /// mode. Every currently-live feed-side provider has exactly one
    /// (`actions/bridges.py` — "assumes a single applicable mode"); a
    /// multi-mode picker is unbuilt and unexercised (`bridges.md` § Link modes
    /// flags it an open unknown).
    fn primary_mode(bridge: &BridgeStatus) -> Option<fauna_protocol::bridges_ui::BridgeLinkMode> {
        Self::platform_modes(bridge).into_iter().next()
    }

    fn link_field(&self, bridge_id: &str, key: &str) -> String {
        self.link_fields
            .get(&(bridge_id.to_string(), key.to_string()))
            .cloned()
            .unwrap_or_default()
    }

    /// The displayed/edited text for a `number` setting: the local buffer if
    /// the user has started editing, else the live nest value stringified —
    /// so both the paint and the FIRST keystroke (`crate::element::Field`'s
    /// generic read) start from what is actually persisted rather than
    /// blanking a populated cap.
    fn number_setting_field(&self, bridge_id: &str, key: &str) -> String {
        if let Some(v) = self
            .number_setting_inputs
            .get(&(bridge_id.to_string(), key.to_string()))
        {
            return v.clone();
        }
        self.bridges
            .iter()
            .find(|b| b.id == bridge_id)
            .and_then(|b| b.settings.iter().find(|s| s.key == key))
            .map(setting_number)
            .unwrap_or_default()
            .to_string()
    }
}

/// Build the page state at the post-auth hook. Deliberately does **not** kick a
/// fetch: entering the tab is the trigger ([`nav_enter_op`], awaited on the nav
/// edge), so a login never pays for a page the user may not open — the same
/// posture Nostr/Media take.
pub fn init(nest: Arc<NestClient>) -> BridgesState {
    BridgesState {
        nest: Some(nest),
        ..BridgesState::default()
    }
}

/// The refetch entering this tab implies — the page's leg of the one nav-edge
/// hook (`crate::app::on_nav_enter`). Returns the op; the caller runs it (the
/// agent awaits, the keyboard spawns), never a fire-and-forget spawn that could
/// race a same-visit mutation.
pub fn nav_enter_op(state: &BridgesState) -> Option<Op> {
    Some(Op::Refresh {
        nest: state.nest.clone()?,
    })
}

// ── Field access ─────────────────────────────────────────────────────────────

/// A Bridges-page editable field. Every one is a local buffer committed by an
/// explicit button (add-follow) or an [`crate::element::Role::InputCommit`]
/// activation (the `number` setting), never per keystroke. All render as
/// untagged (empty-id) inputs — ui.yaml scopes no id to them (module docs).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BridgesField {
    /// A `bridge-link-field-{key}` input, keyed by the bridge and the provider-
    /// declared field key.
    LinkField { bridge_id: String, key: String },
    /// The per-bridge "ID to follow" add-form buffer.
    FollowId { bridge_id: String },
    /// The per-bridge "Petname (optional)" add-form buffer.
    FollowPetname { bridge_id: String },
    /// A `number`-typed [`BridgeSetting`] buffer, keyed by the bridge and the
    /// setting's key (the search-policy cap today — `bridges.md` § Bridge
    /// settings). Committed by [`Action::SetNumberSetting`].
    NumberSetting { bridge_id: String, key: String },
}

pub fn field(state: &BridgesState, field: &BridgesField) -> String {
    match field {
        BridgesField::LinkField { bridge_id, key } => state.link_field(bridge_id, key),
        BridgesField::FollowId { bridge_id } => state
            .follow_id_input
            .get(bridge_id)
            .cloned()
            .unwrap_or_default(),
        BridgesField::FollowPetname { bridge_id } => state
            .follow_petname_input
            .get(bridge_id)
            .cloned()
            .unwrap_or_default(),
        BridgesField::NumberSetting { bridge_id, key } => {
            state.number_setting_field(bridge_id, key)
        }
    }
}

pub fn set_field(state: &mut BridgesState, field: BridgesField, value: String) {
    match field {
        BridgesField::LinkField { bridge_id, key } => {
            state.link_fields.insert((bridge_id, key), value);
        }
        BridgesField::FollowId { bridge_id } => {
            state.follow_id_input.insert(bridge_id, value);
        }
        BridgesField::FollowPetname { bridge_id } => {
            state.follow_petname_input.insert(bridge_id, value);
        }
        BridgesField::NumberSetting { bridge_id, key } => {
            state.number_setting_inputs.insert((bridge_id, key), value);
        }
    }
}

// ── Gestures ─────────────────────────────────────────────────────────────────

/// A gesture on the Bridges page. Each maps onto one `fauna.bridges.*` call or a
/// local buffer change — never a navigation decision of its own.
#[derive(Debug, Clone)]
pub enum Action {
    /// `bridge-action-button` (unlinked) — `fauna.bridges.link` in the bridge's
    /// single mode, then refresh. Params come from the mode's declared
    /// `bridge-link-field-{key}` buffers.
    Link { bridge_id: String, mode: String },
    /// `bridge-action-button` (linked) — `fauna.bridges.unlink`, then refresh.
    Unlink { bridge_id: String },
    /// A boolean setting toggle (untagged) — `set_settings` with just that key.
    /// `value` is the value the tap should PRODUCE, so a click is idempotent
    /// from a driver's side (the Nostr content-flag convention).
    ToggleSetting {
        bridge_id: String,
        key: String,
        value: bool,
    },
    /// A `select` setting (untagged) — carried by index because [`SelectTarget`]
    /// must stay `Copy` and cannot hold the `String` bridge id / key; resolved
    /// back to `(bridge_id, key)` against the live snapshot in [`apply_local`]
    /// (the row-scoped-select convention, e.g. `SelectTarget::FolderConflictPolicy`).
    SetSelectSetting {
        bridge: usize,
        setting: usize,
        value: String,
    },
    /// `bridge-source-request-button` — the ward's "ask your guardian" beside a
    /// `feed_sources` refusal (`family-safety.md` § Feed-source approvals).
    /// Carries the whole triple because that is what the grant is scoped to.
    RequestFeedSource {
        bridge_id: String,
        operation: String,
        target: String,
        label: String,
    },
    /// A `number` setting (untagged) — `set_settings` with the
    /// [`BridgesField::NumberSetting`] buffer, parsed via the shared
    /// [`fauna_core::format::parse_count_i64`] (non-negative only; nest-side
    /// range validation is the real guard — `content-index.md` § Bridge
    /// content in the Search corpus). Committed by
    /// [`crate::element::Role::InputCommit`] activation, the
    /// `folder-member-cap-input` idiom — this IS the tui analogue of the GUI's
    /// debounced auto-save, since a terminal has no keystroke-timer concept
    /// (module docs).
    SetNumberSetting { bridge_id: String, key: String },
    /// `bridge-add-follow-button` — `fauna.bridges.add_follow` from the per-
    /// bridge id/petname buffers, then refresh.
    AddFollow { bridge_id: String },
    /// `bridge-follow-remove` — `fauna.bridges.remove_follow`, keyed by the
    /// follow's own id (not a positional index — a list that shifted under us
    /// must still remove the row the user saw; the Nostr follow-remove rule).
    RemoveFollow { bridge_id: String, id: String },
}

impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback
    /// arm, so a new bridge gesture must answer the offline question.
    ///
    /// Every arm mutates, so none is `None`: the page's own doc comment above
    /// says each action "maps onto one `fauna.bridges.*` call or a local
    /// buffer change", and the local buffer changes are [`BridgesField`]
    /// writes, not gestures.
    pub fn wire_kind(&self) -> Option<&'static str> {
        Some(match self {
            Action::Link { .. } => "fauna.bridges.link",
            Action::Unlink { .. } => "fauna.bridges.unlink",
            // All three setting shapes commit through the one `set_settings`
            // call (see [`apply_local`] — select/number resolve to the same
            // `Op`).
            Action::ToggleSetting { .. }
            | Action::SetSelectSetting { .. }
            | Action::SetNumberSetting { .. } => "fauna.bridges.set_settings",
            Action::AddFollow { .. } => "fauna.bridges.add_follow",
            Action::RemoveFollow { .. } => "fauna.bridges.remove_follow",
            // The one gesture on this page that is NOT a `fauna.bridges.*`
            // call: the ask rides the family namespace, because it is a request
            // about policy rather than an operation on the bridge.
            Action::RequestFeedSource { .. } => "fauna.family.feed_source.request",
        })
    }
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let st = &mut app.bridges;
    match action {
        Action::Link { bridge_id, mode } => {
            let nest = st.nest.clone()?;
            // Build the mode's params from the declared field buffers. A
            // zero-field mode (ActivityPub `enable`) yields an empty map.
            let mut params = BTreeMap::new();
            if let Some(bridge) = st.bridges.iter().find(|b| b.id == bridge_id)
                && let Some(m) = BridgesState::primary_mode(bridge)
            {
                for f in &m.fields {
                    params.insert(
                        f.key.clone(),
                        WireValue::String(st.link_field(&bridge_id, &f.key).trim().to_string()),
                    );
                }
            }
            app.errors.remove(&Page::Bridges);
            Some(Op::Link {
                nest,
                bridge_id,
                mode,
                params: WireValue::Map(params),
            })
        }
        Action::Unlink { bridge_id } => Some(Op::Unlink {
            nest: st.nest.clone()?,
            bridge_id,
        }),
        Action::ToggleSetting {
            bridge_id,
            key,
            value,
        } => Some(Op::SetSettings {
            nest: st.nest.clone()?,
            bridge_id,
            settings: WireValue::Map(BTreeMap::from_iter([(key, WireValue::Bool(value))])),
        }),
        Action::SetSelectSetting {
            bridge,
            setting,
            value,
        } => {
            let nest = st.nest.clone()?;
            // Resolve the indices against the live snapshot — the same shape the
            // row-scoped selects use. A stale index (the list changed under a
            // slow select) simply resolves to nothing and drops the gesture.
            let b = st.bridges.get(bridge)?;
            let key = b.settings.get(setting)?.key.clone();
            Some(Op::SetSettings {
                nest,
                bridge_id: b.id.clone(),
                settings: WireValue::Map(BTreeMap::from_iter([(key, WireValue::String(value))])),
            })
        }
        Action::SetNumberSetting { bridge_id, key } => {
            let nest = st.nest.clone()?;
            // Falls back to the LIVE value (not the raw buffer) the same way
            // the paint does, so an activation with no local edit re-commits
            // the value already shown rather than silently dropping — and a
            // successful parse always yields a wire-sendable number.
            let raw = st.number_setting_field(&bridge_id, &key);
            let n = fauna_core::format::parse_count_i64(&raw)?;
            // Clear the buffer: the repaint after `refresh` is what should own
            // the displayed text from here (the nest may have clamped `n`),
            // matching the page's non-optimistic contract for the other two
            // setting shapes (module docs).
            st.number_setting_inputs
                .remove(&(bridge_id.clone(), key.clone()));
            Some(Op::SetSettings {
                nest,
                bridge_id,
                settings: WireValue::Map(BTreeMap::from_iter([(
                    key,
                    WireValue::Integer(n as i128),
                )])),
            })
        }
        Action::AddFollow { bridge_id } => {
            let nest = st.nest.clone()?;
            let id = st
                .follow_id_input
                .get(&bridge_id)
                .cloned()
                .unwrap_or_default()
                .trim()
                .to_string();
            if id.is_empty() {
                return None;
            }
            let petname = {
                let p = st
                    .follow_petname_input
                    .get(&bridge_id)
                    .cloned()
                    .unwrap_or_default();
                let p = p.trim().to_string();
                (!p.is_empty()).then_some(p)
            };
            st.follow_id_input.remove(&bridge_id);
            st.follow_petname_input.remove(&bridge_id);
            Some(Op::AddFollow {
                nest,
                bridge_id,
                id,
                petname,
            })
        }
        Action::RemoveFollow { bridge_id, id } => Some(Op::RemoveFollow {
            nest: st.nest.clone()?,
            bridge_id,
            id,
        }),
        Action::RequestFeedSource {
            bridge_id,
            operation,
            target,
            label,
        } => Some(Op::RequestFeedSource {
            nest: st.nest.clone()?,
            bridge_id,
            operation,
            target,
            label,
        }),
    }
}

// ── Network half ─────────────────────────────────────────────────────────────

/// The network half of a Bridges gesture — owns only `Arc`s + owned data, so it
/// can be awaited on the agent's path or spawned on the keyboard's.
pub enum Op {
    /// `fauna.bridges.list` (+ `list_follows` per linked follows-capable
    /// bridge) — the nav edge, and after every mutation (the fresh status IS
    /// the observable effect).
    Refresh { nest: Arc<NestClient> },
    Link {
        nest: Arc<NestClient>,
        bridge_id: String,
        mode: String,
        params: WireValue,
    },
    Unlink {
        nest: Arc<NestClient>,
        bridge_id: String,
    },
    SetSettings {
        nest: Arc<NestClient>,
        bridge_id: String,
        settings: WireValue,
    },
    AddFollow {
        nest: Arc<NestClient>,
        bridge_id: String,
        id: String,
        petname: Option<String>,
    },
    RemoveFollow {
        nest: Arc<NestClient>,
        bridge_id: String,
        id: String,
    },
    /// `fauna.family.feed_source.request` — the ward's "ask your guardian"
    /// beside a `feed_sources` refusal (`family-safety.md` § Feed-source
    /// approvals). Re-reads `fauna.family.status` on success so the durable ask
    /// list lands with the ack, the shape the contacts page's ask uses.
    RequestFeedSource {
        nest: Arc<NestClient>,
        bridge_id: String,
        operation: String,
        target: String,
        label: String,
    },
}

/// What an op resolved to.
#[derive(Debug)]
pub enum Outcome {
    /// Fresh unified-page bridge rows + their follows. Every successful op ends
    /// here — the page is non-optimistic by construction, so a painted card
    /// always reflects what the nest actually persisted, never the tap.
    Loaded {
        bridges: Vec<BridgeStatus>,
        follows: BTreeMap<String, Vec<BridgeFollow>>,
    },
    /// A transport or provider failure — lands on `error-message`.
    Failed(String),
    /// The guardian gate refused this operation (the typed
    /// `guardian_approval_required` suffix, never a string match). Carries the
    /// triple so the ask button can be offered for exactly the operation that
    /// was refused, and nothing else.
    ///
    /// The refusal ALSO lands on `error-message`: it is still a real failure,
    /// just not a dead end — clause (b) of the three rules the contacts half
    /// established (`family-safety.md` § Child-initiated contact requests →
    /// *App affordance*).
    RefusedByGuardian {
        bridge_id: String,
        operation: String,
        target: String,
    },
    /// The ask landed. Carries the nest's own re-read list so the pending state
    /// is durable within the session too, not merely across a restart; an empty
    /// list means the re-read failed, which is not a failed ask.
    FeedSourceRequested {
        requests: Vec<fauna_client_family::family::FamilyFeedRequestInfo>,
    },
    /// The ask itself failed (cap reached, knob off — typed refusals the ward
    /// reads verbatim; the guardian gate's own error would mislead here).
    FeedSourceRequestFailed(String),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh { nest } => refresh(nest).await,
            Op::Link {
                nest,
                bridge_id,
                mode,
                params,
            } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.link(bridge_id.clone(), mode, params).await {
                    Ok(_) => refresh(nest).await,
                    // A `Link` ask carries an EMPTY target by construction —
                    // approving a link approves connecting that bridge, and the
                    // OAuth mode is mechanism, not scope
                    // (`FeedSourceOperation::takes_target`).
                    Err(e) => guardian_gate_or_failed(
                        e,
                        bridge_id,
                        fauna_core::data::FeedSourceOperation::Link,
                        String::new(),
                    ),
                }
            }
            Op::Unlink { nest, bridge_id } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.unlink(bridge_id).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::SetSettings {
                nest,
                bridge_id,
                settings,
            } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.set_settings(bridge_id, settings).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::AddFollow {
                nest,
                bridge_id,
                id,
                petname,
            } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client
                    .add_follow(bridge_id.clone(), id.clone(), petname, None)
                    .await
                {
                    Ok(()) => refresh(nest).await,
                    Err(e) => guardian_gate_or_failed(
                        e,
                        bridge_id,
                        fauna_core::data::FeedSourceOperation::Follow,
                        id,
                    ),
                }
            }
            Op::RemoveFollow {
                nest,
                bridge_id,
                id,
            } => {
                let client = BridgesClient::new(Arc::clone(&nest));
                match client.remove_follow(bridge_id, id).await {
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::RequestFeedSource {
                nest,
                bridge_id,
                operation,
                target,
                label,
            } => {
                let client = fauna_client_family::FamilyClient::new(Arc::clone(&nest));
                match client
                    .feed_source_request(bridge_id, operation, target, label)
                    .await
                {
                    // Re-read so the pending state is the nest's own view, not
                    // ours. A failed re-read is not a failed ask — the guardian
                    // has been rung — so it degrades to an empty list and the
                    // local refusal set carries the render.
                    Ok(()) => Outcome::FeedSourceRequested {
                        requests: client
                            .status()
                            .await
                            .map(|s| s.feed_requests)
                            .unwrap_or_default(),
                    },
                    Err(e) => Outcome::FeedSourceRequestFailed(e.to_string()),
                }
            }
        }
    }
}

/// Split the guardian gate off every other failure, on the SHARED typed
/// predicate rather than a per-app string match — the namespace naming *which*
/// handler refused is the nest's business, not this page's
/// (`RpcError::is_guardian_approval_required`; the contacts page's knock send
/// does the same for the same reason).
///
/// Offering the ask only on the *typed* refusal is rule (a) of the three the
/// contacts half established: painting it on a transport failure would tell an
/// unsupervised user their account is supervised.
fn guardian_gate_or_failed(
    e: fauna_client::NestClientError,
    bridge_id: String,
    operation: fauna_core::data::FeedSourceOperation,
    target: String,
) -> Outcome {
    match &e {
        fauna_client::NestClientError::Rpc(err) if err.is_guardian_approval_required() => {
            Outcome::RefusedByGuardian {
                bridge_id,
                operation: operation.as_str().to_string(),
                target,
            }
        }
        _ => Outcome::Failed(e.to_string()),
    }
}

pub(crate) async fn refresh(nest: Arc<NestClient>) -> Outcome {
    let client = BridgesClient::new(Arc::clone(&nest));
    let bridges: Vec<BridgeStatus> = match client.list().await {
        Ok(r) => r.bridges,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    // UNFILTERED — Nostr and Bluesky each have their own dedicated-page
    // consumer, and Bluesky's (`embed_bridge_card`, the Linked panel) needs
    // its real `BridgeStatus` + follows out of this SAME fetch. The shared
    // `is_unified_bridges_page_bridge` exclusion is applied at RENDER time
    // instead (`elements` below) — filtering here would starve a dedicated
    // page's embed exactly the way it once broke linux's Bluesky
    // notification-poll trigger reading this same reply (`ui/atproto.md`
    // § Migration: "apply the predicate at the page, not the fetch").
    //
    // Follows only mean anything for a linked, follows-capable bridge; asking
    // otherwise turns a correct refusal into a page error (the Nostr guard).
    let mut follows = BTreeMap::new();
    for b in &bridges {
        if b.linked && b.supports_follows {
            match client.list_follows(&b.id).await {
                Ok(r) => {
                    follows.insert(b.id.clone(), r.follows);
                }
                Err(e) => return Outcome::Failed(e.to_string()),
            }
        }
    }
    Outcome::Loaded { bridges, follows }
}

/// Fold an op's result back into the page. One function for both dispatch
/// paths, so they cannot disagree about what an outcome means.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    match outcome {
        Outcome::Loaded { bridges, follows } => {
            app.bridges.bridges = bridges;
            app.bridges.follows = follows;
            app.errors.remove(&Page::Bridges);
        }
        Outcome::Failed(msg) => {
            app.errors.insert(Page::Bridges, msg);
        }
        Outcome::RefusedByGuardian {
            bridge_id,
            operation,
            target,
        } => {
            // Clause (b): the refusal STAYS on `error-message`. It is still a
            // real failure — the follow did not happen — and silencing it
            // because an ask is now offered would make the page claim success.
            app.errors
                .insert(Page::Bridges, t::SOURCE_BLOCKED.to_string());
            app.bridges
                .guardian_refused
                .insert((bridge_id, operation, target));
        }
        Outcome::FeedSourceRequested { requests } => {
            if !requests.is_empty() {
                app.family.own_feed_requests = requests;
            }
            app.errors.remove(&Page::Bridges);
        }
        Outcome::FeedSourceRequestFailed(e) => {
            // The ask's own typed refusals are the ward's to read verbatim —
            // the same one path the contacts half carves out, for the same
            // reason: here the guardian gate's own error would mislead.
            app.errors.insert(Page::Bridges, e);
        }
    }
}

// ── Paint ─────────────────────────────────────────────────────────────────────

/// The current value of a boolean setting, defaulting to `false` for any non-
/// boolean payload (a provider that mis-declared a `boolean` setting's value).
fn setting_bool(setting: &BridgeSetting) -> bool {
    matches!(setting.value, WireValue::Bool(true))
}

/// The current value of a string/select setting, or `""` for a non-string
/// payload.
fn setting_str(setting: &BridgeSetting) -> String {
    match &setting.value {
        WireValue::String(s) => s.clone(),
        _ => String::new(),
    }
}

/// The current value of a `number` setting (the search-policy cap), or `0`
/// for a non-integer or out-of-`i64`-range payload (a provider that
/// mis-declared a `number` setting's value).
fn setting_number(setting: &BridgeSetting) -> i64 {
    match setting.value {
        WireValue::Integer(n) => n.try_into().unwrap_or(0),
        _ => 0,
    }
}

pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.bridges;
    let mut out = vec![Element::label(ids::PAGE_HEADING, t::TITLE)];

    // The ONE shared page filter — Nostr and Bluesky own dedicated pages
    // (module docs), never hand-rolled per client. Applied HERE (render time),
    // not in `refresh` — `st.bridges` is the app-wide unfiltered snapshot, so
    // `real_i` (the position `SetSelectSetting` resolves against) survives
    // unchanged while `display_i` renumbers 0-based over just this page's rows
    // (the scoped `bridge-card[i]` e2e contract, unaffected by an earlier
    // Nostr/Bluesky entry in the raw fetch order).
    let unified: Vec<(usize, &BridgeStatus)> = st
        .bridges
        .iter()
        .enumerate()
        .filter(|(_, b)| is_unified_bridges_page_bridge(&b.id))
        .collect();
    if unified.is_empty() {
        // No unified-page bridge on this nest — untagged chrome, exactly like
        // linux's `StatusPage` placeholder (ui.yaml gives the empty-state no
        // id). `is_page_visible()` keys on `page-heading`, which is present.
        out.push(Element::chrome(t::NO_BRIDGES));
        return out;
    }

    for (display_i, (real_i, bridge)) in unified.into_iter().enumerate() {
        bridge_card(st, &app.family, display_i, real_i, bridge, &mut out);
    }
    out
}

/// Embed one bridge's card — link form / linked card / settings / follows —
/// as a DEDICATED page's Linked-account panel (Bluesky today —
/// `ui/atproto.md` § Layout & flow: the shared `bridge-link-form`/`bridge-card`
/// components reused verbatim, zero new element IDs). Reads the SAME app-wide
/// `app.bridges` snapshot the unified Bridges page itself renders from — one
/// `fauna.bridges.list` fetch, shared by reference, so the two surfaces can
/// never disagree (mirrors linux's `sync_linked_panel`, which reads the shared
/// bridges snapshot rather than issuing a second list call). Always at display
/// index 0 — a dedicated page's embed is never one of several cards.
///
/// `bridge_id` absent from the fetch (no reply yet, or a nest built without
/// that provider's feature) still renders the honest unlinked surface — the
/// same "no status yet" case linux's own embed degrades to — rather than
/// nothing at all, so the panel is never silently blank.
pub(crate) fn embed_bridge_card(app: &App, bridge_id: &str, out: &mut Vec<Element>) {
    let st = &app.bridges;
    match st
        .bridges
        .iter()
        .enumerate()
        .find(|(_, b)| b.id == bridge_id)
    {
        Some((real_i, bridge)) => bridge_card(st, &app.family, 0, real_i, bridge, out),
        None => {
            let synthetic = BridgeStatus {
                id: bridge_id.to_string(),
                name: String::new(),
                available: true,
                linked: false,
                identity: None,
                mode: None,
                settings: Vec::new(),
                supports_follows: false,
                supports_follow_requests: false,
                link_modes: None,
                glyph: None,
                error: None,
                extra: Default::default(),
            };
            bridge_card(st, &app.family, 0, 0, &synthetic, out);
        }
    }
}

/// One bridge's inline card — every element registered `.within(ids::BRIDGE_CARD,
/// display_i)` so a scoped query reads exactly this bridge (module docs).
/// `real_i` is this bridge's actual position in `st.bridges` (may differ from
/// `display_i` when earlier entries were filtered out of this rendering, or
/// when a dedicated page embeds a single card at `display_i` 0) — the only
/// consumer is `setting_elements`'s `SetSelectSetting`, which resolves back
/// against the real, unfiltered list.
fn bridge_card(
    st: &BridgesState,
    fam: &crate::family::FamilyState,
    display_i: usize,
    real_i: usize,
    bridge: &BridgeStatus,
    out: &mut Vec<Element>,
) {
    // The bridge name — untagged chrome (ui.yaml scopes no per-card name id).
    out.push(Element::chrome(bridge.name.clone()).within(ids::BRIDGE_CARD, display_i));

    if bridge.linked {
        // Identity ("Handle: @user@domain") — chrome; ui.yaml scopes no id.
        if let Some(identity) = &bridge.identity {
            out.push(
                Element::chrome(format!("{}: {}", identity.label, identity.display))
                    .within(ids::BRIDGE_CARD, display_i),
            );
        }
        // The dual-purpose action button, in its Unlink role. Registered before
        // settings/follows so the shared unscoped `click("bridge-action-button",
        // index=i)` still lands on it.
        out.push(
            Element::gesture_button(
                ids::BRIDGE_ACTION_BUTTON,
                t::UNLINK_BRIDGE,
                true,
                Gesture::Bridges(Action::Unlink {
                    bridge_id: bridge.id.clone(),
                }),
            )
            .within(ids::BRIDGE_CARD, display_i),
        );
        setting_elements(st, display_i, real_i, bridge, out);
        if bridge.supports_follows {
            follows_elements(st, display_i, bridge, out);
        }
    } else {
        // The unlinked link form: one input per declared field, then the Link
        // button — ALWAYS rendered, even with no declared mode at all (a
        // dedicated-page embed of a bridge the nest hasn't registered a
        // provider for yet — `embed_bridge_card`'s synthetic-status case).
        // Matches linux's `build_bridge_detail_content`, whose Link/Unlink
        // pair gates only on `linked`, never on whether modes are declared.
        // A zero-field mode (ActivityPub `enable`) renders just the button
        // either way.
        //
        // When no mode applies the button stays rendered but goes DISABLED and
        // a reason renders beside it (`bridges.md` § Errors & edge cases —
        // disabled, not absent, and never live-but-inert). Before this, tui
        // dispatched `fauna.bridges.link` with an empty `mode` and let the nest
        // refuse — a round trip that threw away the specific explanation the
        // nest had already sent in `error` for a generic one.
        let block = fauna_client_bridges::link_block(
            bridge,
            // The count AFTER the shared platform filter — feeding the raw
            // declared count here is what made a web-only mode read as
            // linkable on tui.
            BridgesState::platform_modes(bridge).len(),
        );
        if let Some(mode) = BridgesState::primary_mode(bridge) {
            for f in &mode.fields {
                out.push(
                    Element::input(
                        format!("bridge-link-field-{}", f.key),
                        st.link_field(&bridge.id, &f.key),
                        Field::Bridges(BridgesField::LinkField {
                            bridge_id: bridge.id.clone(),
                            key: f.key.clone(),
                        }),
                    )
                    .labelled(f.label.clone())
                    .within(ids::BRIDGE_CARD, display_i),
                );
            }
        }
        if let Some(reason) = &block {
            out.push(
                Element::label(ids::BRIDGE_LINK_BLOCKED_REASON, reason.text())
                    .within(ids::BRIDGE_CARD, display_i),
            );
        }
        out.push(
            Element::gesture_button(
                ids::BRIDGE_ACTION_BUTTON,
                t::LINK_BRIDGE,
                block.is_none(),
                Gesture::Bridges(Action::Link {
                    bridge_id: bridge.id.clone(),
                    mode: BridgesState::primary_mode(bridge)
                        .map(|m| m.mode.clone())
                        .unwrap_or_default(),
                }),
            )
            .within(ids::BRIDGE_CARD, display_i),
        );
    }
    // One call per card, covering every operation: a card can carry a blocked
    // link and blocked follows at once, and they are independent asks.
    source_ask_rows(fam, st, bridge, display_i, out);
}

/// The metadata-driven settings rows for a linked bridge — untagged (ui.yaml
/// scopes no per-setting id), so they are keyboard-focusable but not
/// automatable (module docs). Booleans render as a toggle, `select` as a
/// picker, `number` as a committing input (the search-policy cap —
/// `content-index.md` § Bridge content in the Search corpus); any other type
/// falls back to a read-only "label: value" line rather than an unbuilt
/// input-commit path (no live provider declares a free-text setting on this
/// page — priority #5).
fn setting_elements(
    st: &BridgesState,
    display_i: usize,
    real_i: usize,
    bridge: &BridgeStatus,
    out: &mut Vec<Element>,
) {
    for (s_idx, setting) in bridge.settings.iter().enumerate() {
        match setting.setting_type.as_str() {
            "boolean" | "toggle" | "bool" => {
                let on = setting_bool(setting);
                out.push(
                    Element::checkbox_gesture(
                        String::new(),
                        setting.label.clone(),
                        on,
                        Gesture::Bridges(Action::ToggleSetting {
                            bridge_id: bridge.id.clone(),
                            key: setting.key.clone(),
                            value: !on,
                        }),
                    )
                    .within(ids::BRIDGE_CARD, display_i),
                );
            }
            "select" => {
                let options = setting
                    .options
                    .as_ref()
                    .map(|opts| {
                        opts.iter()
                            .filter_map(|o| match &o.value {
                                WireValue::String(s) => Some(s.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                out.push(
                    Element::select(
                        String::new(),
                        setting_str(setting),
                        // `real_i` — this is resolved back against the
                        // unfiltered `st.bridges` by `apply_local`'s
                        // `SetSelectSetting` arm, not the display position.
                        SelectTarget::BridgeSetting {
                            bridge: real_i,
                            setting: s_idx,
                        },
                        options,
                    )
                    .labelled(setting.label.clone())
                    .within(ids::BRIDGE_CARD, display_i),
                );
            }
            SETTING_TYPE_NUMBER => {
                out.push(
                    Element::input_commit(
                        String::new(),
                        st.number_setting_field(&bridge.id, &setting.key),
                        Field::Bridges(BridgesField::NumberSetting {
                            bridge_id: bridge.id.clone(),
                            key: setting.key.clone(),
                        }),
                        Gesture::Bridges(Action::SetNumberSetting {
                            bridge_id: bridge.id.clone(),
                            key: setting.key.clone(),
                        }),
                    )
                    .labelled(setting.label.clone())
                    .within(ids::BRIDGE_CARD, display_i),
                );
            }
            _ => {
                out.push(
                    Element::chrome(format!("{}: {}", setting.label, setting_str(setting)))
                        .within(ids::BRIDGE_CARD, display_i),
                );
            }
        }
    }
}

/// The follows surface for a `supports_follows` bridge — the `bridge-follows-
/// list` anchor, one `bridge-follow-item` + `bridge-follow-remove` per follow,
/// then the untagged id/petname add inputs and the `bridge-add-follow-button`.
/// The ward's feed-source ask rows for ONE bridge card — the
/// `bridge-source-request-button` / `bridge-source-request-state` pair
/// (`family-safety.md` § Feed-source approvals). Both are `indexed` in ui.yaml
/// because a card can carry several: a blocked link AND two blocked follows are
/// three independent asks, each with its own verdict.
///
/// Pushes nothing at all in the common case, which is the whole design: it
/// paints only where the guardian gate has actually bitten. **No explicit
/// "is supervised" test is needed and none is written** — both inputs are
/// supervised-only by construction (`own_feed_requests` is gated on
/// `supervised_by` where the status read folds it in; `guardian_refused` fills
/// only on the nest's TYPED refusal), so an unsupervised account cannot reach
/// either branch. Deriving the gate from the data rather than re-testing it
/// means this surface cannot drift out of step with what the nest enforces.
///
/// Keyed on the ask data itself, never on the add-form buffers: `apply_local`
/// CLEARS `follow_id_input`/`follow_petname_input` when it dispatches the
/// follow, so by the time a refusal comes back the id the user typed is already
/// gone. A buffer-keyed row would therefore never paint at all.
///
/// Durable rows first, then session-only refusals — the contacts page's
/// ordering, for its reason: the durable list survives navigation and restart,
/// and is what makes the state honest on a fresh session that never saw the
/// refusal. A triple with a durable row is skipped in the second pass, so an
/// answered ask shows its verdict rather than offering the button again.
fn source_ask_rows(
    fam: &crate::family::FamilyState,
    st: &BridgesState,
    bridge: &BridgeStatus,
    card_index: usize,
    out: &mut Vec<Element>,
) {
    for ask in fam
        .own_feed_requests
        .iter()
        .filter(|r| r.bridge_id == bridge.id)
    {
        let text = if ask.approved_at.is_some() {
            // The prompt, not a retry: an approval is a single-use grant the
            // ward redeems by retrying, so spending it on a render the user did
            // not ask for would burn it — and a lapsed grant would then read as
            // a silent failure (§ Feed-source approvals).
            t::SOURCE_REQUEST_APPROVED
        } else {
            t::SOURCE_REQUEST_PENDING
        };
        out.push(
            Element::label(ids::BRIDGE_SOURCE_REQUEST_STATE, text.to_string())
                .within(ids::BRIDGE_CARD, card_index),
        );
    }
    for (b, operation, target) in &st.guardian_refused {
        if b != &bridge.id {
            continue;
        }
        if fam.feed_request_state(b, operation, target).is_some() {
            continue;
        }
        out.push(
            Element::gesture_button(
                ids::BRIDGE_SOURCE_REQUEST_BUTTON,
                t::SOURCE_REQUEST_BUTTON,
                true,
                Gesture::Bridges(Action::RequestFeedSource {
                    bridge_id: b.clone(),
                    operation: operation.clone(),
                    target: target.clone(),
                    // Display-only, and the bridge name is the one label still
                    // available: the follow's petname buffer is cleared at
                    // dispatch (above), so there is nothing more specific to
                    // carry without inventing it.
                    label: bridge.name.clone(),
                }),
            )
            .within(ids::BRIDGE_CARD, card_index),
        );
    }
}

fn follows_elements(st: &BridgesState, i: usize, bridge: &BridgeStatus, out: &mut Vec<Element>) {
    out.push(Element::label(ids::BRIDGE_FOLLOWS_LIST, t::FOLLOWS).within(ids::BRIDGE_CARD, i));
    if let Some(follows) = st.follows.get(&bridge.id) {
        for f in follows {
            out.push(
                Element::label(ids::BRIDGE_FOLLOW_ITEM, follow_display(f))
                    .within(ids::BRIDGE_CARD, i),
            );
            out.push(
                Element::gesture_button(
                    ids::BRIDGE_FOLLOW_REMOVE,
                    common::REMOVE,
                    true,
                    Gesture::Bridges(Action::RemoveFollow {
                        bridge_id: bridge.id.clone(),
                        id: f.id.clone(),
                    }),
                )
                .within(ids::BRIDGE_CARD, i),
            );
        }
    }
    // Untagged add-form inputs — ui.yaml gives the id/petname fields no id
    // (module docs); the button carries the only id in this add row.
    out.push(
        Element::input(
            String::new(),
            st.follow_id_input
                .get(&bridge.id)
                .cloned()
                .unwrap_or_default(),
            Field::Bridges(BridgesField::FollowId {
                bridge_id: bridge.id.clone(),
            }),
        )
        .labelled(t::ID_TO_FOLLOW)
        .within(ids::BRIDGE_CARD, i),
    );
    out.push(
        Element::input(
            String::new(),
            st.follow_petname_input
                .get(&bridge.id)
                .cloned()
                .unwrap_or_default(),
            Field::Bridges(BridgesField::FollowPetname {
                bridge_id: bridge.id.clone(),
            }),
        )
        .labelled(t::PETNAME_OPTIONAL)
        .within(ids::BRIDGE_CARD, i),
    );
    out.push(
        Element::gesture_button(
            ids::BRIDGE_ADD_FOLLOW_BUTTON,
            t::ADD_FOLLOW,
            true,
            Gesture::Bridges(Action::AddFollow {
                bridge_id: bridge.id.clone(),
            }),
        )
        .within(ids::BRIDGE_CARD, i),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::authed_app;
    use fauna_client_family::family::FamilyFeedRequestInfo;
    use fauna_protocol::bridges_ui::{
        BridgeIdentity, BridgeLinkField, BridgeLinkMode, BridgeSettingOption,
    };

    fn dummy_nest() -> Arc<NestClient> {
        NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        )
    }

    fn setting(key: &str, type_: &str, value: WireValue) -> BridgeSetting {
        BridgeSetting {
            key: key.to_string(),
            label: format!("{key} label"),
            setting_type: type_.to_string(),
            value,
            options: None,
            extra: Default::default(),
        }
    }

    fn select_setting(key: &str, value: &str, opts: &[&str]) -> BridgeSetting {
        BridgeSetting {
            key: key.to_string(),
            label: format!("{key} label"),
            setting_type: "select".to_string(),
            value: WireValue::String(value.to_string()),
            options: Some(
                opts.iter()
                    .map(|o| BridgeSettingOption {
                        value: WireValue::String(o.to_string()),
                        label: o.to_uppercase(),
                        extra: Default::default(),
                    })
                    .collect(),
            ),
            extra: Default::default(),
        }
    }

    // ── Ward-side feed-source asks (family-safety.md § Feed-source approvals) ──
    //
    // What these pin, and why each is not vacuous: the surface has exactly two
    // inputs (the durable `own_feed_requests` list and the session-local
    // `guardian_refused` set) and one hard product rule (an approval is a GRANT
    // the ward redeems by retrying — never an auto-retry). A test that only
    // asserted "a row appeared" would pass against a surface that painted the
    // button unconditionally, which is precisely the failure the gate exists to
    // prevent.
    //
    // ⚠ The fixture bridge is `activitypub`, never `nostr`/`bluesky`: `elements()`
    // filters the page through `is_unified_bridges_page_bridge`, which drops
    // those two (they own dedicated pages). A `nostr` fixture renders NO card,
    // so every assertion here — the negative ones included — would pass
    // vacuously against any implementation at all. Measured: the first draft of
    // these tests did exactly that.

    fn a_feed_ask(
        bridge_id: &str,
        operation: &str,
        target: &str,
        approved: bool,
    ) -> FamilyFeedRequestInfo {
        FamilyFeedRequestInfo {
            bridge_id: bridge_id.to_string(),
            operation: operation.to_string(),
            target: target.to_string(),
            label: String::new(),
            created_at: 1,
            approved_at: approved.then_some(2),
            extra: Default::default(),
        }
    }

    /// An account that never hit the gate shows NEITHER element. This is the
    /// unsupervised case too: both inputs are supervised-only by construction,
    /// so "no rows" and "not supervised" are the same state here.
    #[test]
    fn no_refusal_and_no_ask_renders_no_source_elements() {
        let mut app = authed_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", Vec::new())];

        let out = elements(&app);

        assert!(
            !out.iter().any(|e| e.id == ids::BRIDGE_SOURCE_REQUEST_BUTTON
                || e.id == ids::BRIDGE_SOURCE_REQUEST_STATE),
            "the ask surface must paint only where the guardian gate actually bit; \
             an unconditional render would tell every unsupervised user their \
             account is supervised (family-safety.md § Feed-source approvals, \
             rule (a) of the contacts half)"
        );
    }

    /// A typed refusal this session offers the ask — and offers it for the
    /// refused triple only.
    #[test]
    fn a_guardian_refusal_offers_the_ask_button() {
        let mut app = authed_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", Vec::new())];
        apply_outcome(
            &mut app,
            Outcome::RefusedByGuardian {
                bridge_id: "activitypub".to_string(),
                operation: "follow".to_string(),
                target: "npub1abc".to_string(),
            },
        );

        let out = elements(&app);

        assert_eq!(
            out.iter()
                .filter(|e| e.id == ids::BRIDGE_SOURCE_REQUEST_BUTTON)
                .count(),
            1,
            "the refused follow must offer exactly one ask"
        );
        // Clause (b): the refusal is still a real failure and stays on
        // `error-message` — it is just no longer a dead end.
        assert_eq!(
            app.errors.get(&Page::Bridges).map(String::as_str),
            Some(t::SOURCE_BLOCKED),
            "the refusal must remain on error-message, and must NOT borrow \
             contacts' 'can only message approved contacts' text — this is \
             about sources, not people"
        );
    }

    /// The durable list wins over the session flag: once the ask has landed,
    /// the row shows its verdict instead of offering the button again.
    #[test]
    fn a_landed_ask_replaces_the_button_with_its_state() {
        let mut app = authed_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", Vec::new())];
        apply_outcome(
            &mut app,
            Outcome::RefusedByGuardian {
                bridge_id: "activitypub".to_string(),
                operation: "follow".to_string(),
                target: "npub1abc".to_string(),
            },
        );
        app.family.own_feed_requests = vec![a_feed_ask("activitypub", "follow", "npub1abc", false)];

        let out = elements(&app);

        assert!(
            !out.iter()
                .any(|e| e.id == ids::BRIDGE_SOURCE_REQUEST_BUTTON),
            "a triple with a durable ask must stop offering the button, else the \
             ward can ring the guardian repeatedly for one already-pending ask"
        );
        let state: Vec<&str> = out
            .iter()
            .filter(|e| e.id == ids::BRIDGE_SOURCE_REQUEST_STATE)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(state, vec![t::SOURCE_REQUEST_PENDING]);
    }

    /// ⚠ The product rule with teeth: an APPROVED grant prompts the retry and
    /// the app never retries on its own. Asserting the *text* is what makes
    /// this non-vacuous — a surface that auto-retried would still render a row.
    #[test]
    fn an_approved_ask_prompts_the_retry_and_never_auto_retries() {
        let mut app = authed_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", Vec::new())];
        app.family.own_feed_requests = vec![a_feed_ask("activitypub", "follow", "npub1abc", true)];

        let out = elements(&app);

        let state: Vec<&str> = out
            .iter()
            .filter(|e| e.id == ids::BRIDGE_SOURCE_REQUEST_STATE)
            .map(|e| e.text.as_str())
            .collect();
        assert_eq!(
            state,
            vec![t::SOURCE_REQUEST_APPROVED],
            "approved must render the TRY AGAIN prompt (family-safety.md \
             § Feed-source approvals: 'an approval is a grant the ward redeems \
             by retrying; the app never auto-retries')"
        );
        // The approved state is a LABEL, never a button: rendering it as an
        // affordance is the first step toward redeeming the grant on paint. A
        // grant is single-use, so auto-redeeming burns it on a navigation the
        // user did not ask for, and the lapse then reads as a silent failure.
        // The retry the ward makes is the ORIGINAL gesture (Add Follow / Link),
        // which is still on the card and unchanged.
        assert!(
            !out.iter()
                .any(|e| e.id == ids::BRIDGE_SOURCE_REQUEST_BUTTON),
            "an approved ask must not still offer 'ask your guardian'"
        );
        assert!(
            out.iter().any(|e| e.id == ids::BRIDGE_ADD_FOLLOW_BUTTON),
            "the ward redeems the grant by retrying the original operation, so \
             that gesture must still be on the card"
        );
    }

    /// The grant is scoped to the whole `(bridge, operation, target)` triple —
    /// a different follow on the same bridge is a different ask.
    #[test]
    fn the_ask_state_is_keyed_on_the_whole_triple() {
        let mut app = authed_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", Vec::new())];
        app.family.own_feed_requests = vec![a_feed_ask("activitypub", "follow", "npub1abc", true)];

        assert_eq!(
            app.family
                .feed_request_state("activitypub", "follow", "npub1abc"),
            Some(crate::family::FeedRequestState::Approved)
        );
        assert_eq!(
            app.family
                .feed_request_state("activitypub", "follow", "npub1other"),
            None,
            "one follow's grant must not light up another follow's row"
        );
        assert_eq!(
            app.family.feed_request_state("activitypub", "link", ""),
            None,
            "a follow grant must not satisfy the bridge's LINK gate — a link ask \
             carries an empty target by construction"
        );
    }

    /// A graduated account drops its asks: no guardian to be waiting on, so a
    /// stale grant must not keep prompting a retry on a page that now links
    /// freely. (The nest drops the rows too; this is the client half.)
    #[test]
    fn an_unsupervised_account_renders_no_stale_ask() {
        let mut app = authed_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", Vec::new())];
        app.family.own_feed_requests = vec![a_feed_ask("activitypub", "follow", "npub1abc", true)];
        // What the status read does for an account with no guardian.
        app.family.own_feed_requests = Vec::new();

        let out = elements(&app);

        assert!(
            !out.iter().any(|e| e.id == ids::BRIDGE_SOURCE_REQUEST_STATE),
            "a graduated account must show no leftover ask state"
        );
    }

    /// A linked bridge: an identity, settings, and (by default) follows support.
    fn linked_bridge(id: &str, name: &str, settings: Vec<BridgeSetting>) -> BridgeStatus {
        BridgeStatus {
            id: id.to_string(),
            name: name.to_string(),
            available: true,
            linked: true,
            identity: Some(BridgeIdentity {
                label: "Handle".to_string(),
                value: format!("https://{id}.example"),
                display: format!("@user@{id}"),
                extra: Default::default(),
            }),
            mode: Some("enabled".to_string()),
            settings,
            supports_follows: true,
            supports_follow_requests: false,
            link_modes: None,
            glyph: None,
            error: None,
            extra: Default::default(),
        }
    }

    /// An unlinked bridge declaring one link mode with `fields` (empty for the
    /// zero-field ActivityPub `enable` vehicle).
    fn unlinked_bridge(id: &str, name: &str, mode: &str, field_keys: &[&str]) -> BridgeStatus {
        BridgeStatus {
            id: id.to_string(),
            name: name.to_string(),
            available: true,
            linked: false,
            identity: None,
            mode: None,
            settings: vec![],
            supports_follows: true,
            supports_follow_requests: false,
            link_modes: Some(vec![BridgeLinkMode {
                mode: mode.to_string(),
                label: format!("{mode} label"),
                client_action: None,
                platform: None,
                fields: field_keys
                    .iter()
                    .map(|k| BridgeLinkField {
                        key: k.to_string(),
                        label: format!("{k} label"),
                        field_type: "text_input".to_string(),
                        placeholder: None,
                        extra: Default::default(),
                    })
                    .collect(),
                extra: Default::default(),
            }]),
            glyph: None,
            error: None,
            extra: Default::default(),
        }
    }

    fn bridges_app() -> App {
        let mut app = authed_app();
        app.page = Page::Bridges;
        app.bridges.nest = Some(dummy_nest());
        app
    }

    fn count_id(app: &App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    /// Count occurrences of `id` scoped to one `bridge-card[index]` — the
    /// registry's own containment rule, asked through `Registry` itself, so this
    /// proves the scoped e2e query would read exactly that bridge.
    fn count_scoped(app: &App, id: &str, index: usize) -> usize {
        crate::automation::Registry::of(elements(app))
            .count_scoped(id, &[("bridge-card".to_string(), index)])
    }

    /// The ActivityPub e2e vehicle: an unlinked, zero-field `enable` bridge
    /// paints the page heading + exactly one action button and NO link fields —
    /// the shape `actions/bridges.py::link()` drives with an empty `fields` map.
    #[test]
    fn unlinked_zero_field_bridge_renders_only_the_action_button() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![unlinked_bridge("activitypub", "ActivityPub", "enable", &[])];

        assert_eq!(count_id(&app, "page-heading"), 1);
        assert_eq!(count_id(&app, "bridge-action-button"), 1);
        assert_eq!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "bridge-action-button")
                .unwrap()
                .text,
            t::LINK_BRIDGE
        );
        // No fields for `enable`, and none of the linked-only elements.
        assert!(
            !elements(&app)
                .iter()
                .any(|e| e.id.starts_with("bridge-link-field-"))
        );
        assert_eq!(count_id(&app, "bridge-follows-list"), 0);
    }

    /// A mode scoped to ANOTHER platform neither counts as applicable nor
    /// renders its fields on tui (the gap this filter closed). tui
    /// had NO platform filter before 2026-08-15: Nostr's `platform: "web"`
    /// NIP-07 mode counted toward `link_block`'s applicable count and, as the
    /// first declared mode, became `primary_mode` — rendering a form whose
    /// `client_action` (a browser extension) a terminal cannot perform, the
    /// live-but-inert control the link-block rule exists to prevent. With
    /// every declared mode scoped elsewhere, the correct render is the
    /// DISABLED button + the localized no-method reason.
    #[test]
    fn a_mode_scoped_to_another_platform_neither_counts_nor_renders_fields() {
        let mut app = bridges_app();
        // An id the unified Bridges page actually lists ("nostr" would be
        // excluded wholesale — it has its own dedicated page, bridges.md
        // § Scope — and the whole card would vacuously not render).
        let mut bridge = unlinked_bridge("activitypub", "ActivityPub", "nip07", &["pubkey"]);
        bridge
            .link_modes
            .as_mut()
            .unwrap()
            .iter_mut()
            .for_each(|m| m.platform = Some("web".to_string()));
        app.bridges.bridges = vec![bridge];

        // The web-only mode's fields must NOT render...
        assert!(
            !elements(&app)
                .iter()
                .any(|e| e.id.starts_with("bridge-link-field-")),
            "a web-scoped mode's fields rendered on tui"
        );
        // ...and the action button renders BLOCKED (disabled + reason), the
        // same shape as zero declared modes — because for tui that is what it
        // is.
        assert_eq!(count_id(&app, "bridge-action-button"), 1);
        assert_eq!(count_id(&app, "bridge-link-blocked-reason"), 1);
        assert_eq!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "bridge-link-blocked-reason")
                .unwrap()
                .text,
            t::NO_LINK_METHOD
        );
    }

    /// A bridge in the nest's degraded shape (`provider.status()` errored:
    /// `link_modes=None, error=Some(why)`) keeps its action button — rendered
    /// DISABLED, never absent — and paints the nest's own sentence beside it.
    ///
    /// Before this, tui rendered a live button and dispatched
    /// `fauna.bridges.link` with an empty `mode`, throwing away the specific
    /// explanation the nest had already sent for a generic round-trip refusal.
    #[test]
    fn a_degraded_bridge_disables_link_and_shows_the_nests_own_reason() {
        let mut app = bridges_app();
        let mut degraded = unlinked_bridge("activitypub", "ActivityPub", "enable", &[]);
        degraded.link_modes = None;
        degraded.error = Some("relay unreachable: connection refused".to_string());
        app.bridges.bridges = vec![degraded];

        // Disabled, not absent (`bridges.md` § Errors & edge cases).
        let button = elements(&app)
            .into_iter()
            .find(|e| e.id == "bridge-action-button")
            .expect("the action button stays rendered when linking is blocked");
        assert!(
            !button.enabled,
            "a bridge with no applicable mode must not offer a live Link control"
        );

        // The nest's explanation, verbatim and scoped to this card.
        assert_eq!(count_scoped(&app, "bridge-link-blocked-reason", 0), 1);
        assert_eq!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "bridge-link-blocked-reason")
                .unwrap()
                .text,
            "relay unreachable: connection refused"
        );
    }

    /// Degraded with no `error` on the wire: still blocked, but the reason falls
    /// back to the localized generic — never an empty line, never a live button.
    #[test]
    fn a_degraded_bridge_without_an_error_falls_back_to_the_generic_reason() {
        let mut app = bridges_app();
        let mut degraded = unlinked_bridge("activitypub", "ActivityPub", "enable", &[]);
        degraded.link_modes = None;
        app.bridges.bridges = vec![degraded];

        assert!(
            !elements(&app)
                .into_iter()
                .find(|e| e.id == "bridge-action-button")
                .unwrap()
                .enabled
        );
        assert_eq!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "bridge-link-blocked-reason")
                .unwrap()
                .text,
            t::NO_LINK_METHOD
        );
    }

    /// The guard against over-firing: an ordinary linkable bridge keeps a LIVE
    /// button and paints no reason at all.
    #[test]
    fn a_linkable_bridge_is_untouched_by_the_block() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![unlinked_bridge("activitypub", "ActivityPub", "enable", &[])];

        assert!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "bridge-action-button")
                .unwrap()
                .enabled
        );
        assert_eq!(count_id(&app, "bridge-link-blocked-reason"), 0);
    }

    /// A declared field renders as `bridge-link-field-{key}`, and linking reads
    /// that field's buffer into the `fauna.bridges.link` params.
    #[test]
    fn a_declared_field_renders_and_feeds_the_link_params() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![unlinked_bridge("bsky", "Bluesky", "oauth", &["handle"])];
        assert_eq!(count_id(&app, "bridge-link-field-handle"), 1);

        set_field(
            &mut app.bridges,
            BridgesField::LinkField {
                bridge_id: "bsky".to_string(),
                key: "handle".to_string(),
            },
            "  me.bsky.social  ".to_string(),
        );
        let op = apply_local(
            &mut app,
            Action::Link {
                bridge_id: "bsky".to_string(),
                mode: "oauth".to_string(),
            },
        )
        .expect("link returns an op");
        match op {
            Op::Link {
                bridge_id,
                mode,
                params,
                ..
            } => {
                assert_eq!(bridge_id, "bsky");
                assert_eq!(mode, "oauth");
                let WireValue::Map(m) = params else {
                    panic!("params is a map")
                };
                // Trimmed, keyed by the field key.
                assert_eq!(
                    m.get("handle"),
                    Some(&WireValue::String("me.bsky.social".to_string()))
                );
            }
            _ => panic!("expected Op::Link"),
        }
    }

    /// A linked bridge paints the Unlink button FIRST (so the shared unscoped
    /// `click("bridge-action-button", index=0)` lands on it) plus the follows
    /// surface; the action button carries the Unlink gesture.
    #[test]
    fn a_linked_bridge_paints_unlink_and_follows() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", vec![])];

        let els = elements(&app);
        let action_idx = els
            .iter()
            .position(|e| e.id == "bridge-action-button")
            .unwrap();
        let follows_idx = els
            .iter()
            .position(|e| e.id == "bridge-follows-list")
            .unwrap();
        assert!(
            action_idx < follows_idx,
            "the action button registers before the follows"
        );
        assert_eq!(els[action_idx].text, t::UNLINK_BRIDGE);
        assert_eq!(count_id(&app, "bridge-add-follow-button"), 1);
    }

    /// Follows on two bridges must be scope-addressable per bridge — the A6
    /// nesting lesson: a scoped `count("bridge-follow-item", scope="bridge-card[i]")`
    /// reads exactly that bridge, never the global total.
    #[test]
    fn follows_are_scoped_per_bridge_card() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![
            linked_bridge("activitypub", "ActivityPub", vec![]),
            linked_bridge("other", "Other", vec![]),
        ];
        app.bridges.follows.insert(
            "activitypub".to_string(),
            vec![
                BridgeFollow {
                    id: "a".to_string(),
                    petname: Some("Alice".to_string()),
                    created_at: None,
                    extra: None,
                    unknown_keys: Default::default(),
                },
                BridgeFollow {
                    id: "b".to_string(),
                    petname: None,
                    created_at: None,
                    extra: None,
                    unknown_keys: Default::default(),
                },
            ],
        );
        app.bridges.follows.insert(
            "other".to_string(),
            vec![BridgeFollow {
                id: "c".to_string(),
                petname: None,
                created_at: None,
                extra: None,
                unknown_keys: Default::default(),
            }],
        );

        assert_eq!(
            count_id(&app, "bridge-follow-item"),
            3,
            "global count is the sum"
        );
        assert_eq!(count_scoped(&app, "bridge-follow-item", 0), 2);
        assert_eq!(count_scoped(&app, "bridge-follow-item", 1), 1);
        // The petname wins over the raw id in the display.
        assert!(
            elements(&app)
                .iter()
                .any(|e| e.id == "bridge-follow-item" && e.text == "Alice")
        );
    }

    /// A boolean setting toggle carries the FLIPPED value, so a click sets the
    /// opposite of what is currently persisted (non-optimistic).
    #[test]
    fn a_boolean_setting_toggle_carries_the_flipped_value() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge(
            "activitypub",
            "ActivityPub",
            vec![setting(
                "auto_accept_follows",
                "boolean",
                WireValue::Bool(true),
            )],
        )];
        // The toggle is untagged (no ui.yaml id) but present as a focusable
        // checkbox — find it by its label.
        let toggle = elements(&app)
            .into_iter()
            .find(|e| e.text == "auto_accept_follows label")
            .expect("the boolean setting renders");
        let gesture = match toggle.role {
            crate::element::Role::Checkbox { gesture, checked } => {
                assert!(checked, "reads the persisted `true`");
                gesture
            }
            _ => panic!("a boolean setting is a checkbox"),
        };
        match gesture {
            Gesture::Bridges(Action::ToggleSetting { key, value, .. }) => {
                assert_eq!(key, "auto_accept_follows");
                assert!(!value, "the tap produces the flipped value");
            }
            _ => panic!("expected a ToggleSetting gesture"),
        }
    }

    /// A `select` setting resolves its (bridge, setting) indices back to the key
    /// when committed — the Copy-safe indirection.
    #[test]
    fn a_select_setting_resolves_indices_to_its_key() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge(
            "activitypub",
            "ActivityPub",
            vec![
                setting("auto_accept_follows", "boolean", WireValue::Bool(false)),
                select_setting("default_visibility", "public", &["public", "unlisted"]),
            ],
        )];
        // The select renders (untagged) with the persisted value + the option
        // values (not labels — the value round-trips through `select`).
        let sel = elements(&app)
            .into_iter()
            .find(|e| matches!(e.role, crate::element::Role::Select { .. }))
            .expect("the select setting renders");
        assert_eq!(sel.text, "public");

        let op = apply_local(
            &mut app,
            Action::SetSelectSetting {
                bridge: 0,
                setting: 1,
                value: "unlisted".to_string(),
            },
        )
        .expect("a select commit returns an op");
        match op {
            Op::SetSettings {
                bridge_id,
                settings,
                ..
            } => {
                assert_eq!(bridge_id, "activitypub");
                let WireValue::Map(m) = settings else {
                    panic!("settings is a map")
                };
                assert_eq!(
                    m.get("default_visibility"),
                    Some(&WireValue::String("unlisted".to_string()))
                );
            }
            _ => panic!("expected Op::SetSettings"),
        }
    }

    /// A `number` setting with no local edit paints the LIVE value (not a
    /// blank buffer) as a committing input — the same live-fallback the first
    /// keystroke needs, proven at the render layer. Uses `activitypub`, not
    /// `nostr` — `nostr`/`bluesky` are filtered off THIS page (module docs;
    /// `elements()`'s `is_unified_bridges_page_bridge` predicate), so a
    /// render-level assertion against them would vacuously find nothing.
    #[test]
    fn a_number_setting_with_no_edit_paints_the_live_value() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge(
            "activitypub",
            "ActivityPub",
            vec![setting(
                "limit_posts_in_search",
                SETTING_TYPE_NUMBER,
                WireValue::Integer(1000),
            )],
        )];
        let input = elements(&app)
            .into_iter()
            .find(|e| matches!(e.role, crate::element::Role::InputCommit { .. }))
            .expect("the number setting renders as a committing input");
        assert_eq!(input.text, "1000");
        match input.role {
            crate::element::Role::InputCommit { field, gesture } => {
                assert_eq!(
                    field,
                    Field::Bridges(BridgesField::NumberSetting {
                        bridge_id: "activitypub".to_string(),
                        key: "limit_posts_in_search".to_string(),
                    })
                );
                match gesture {
                    Gesture::Bridges(Action::SetNumberSetting { bridge_id, key }) => {
                        assert_eq!(bridge_id, "activitypub");
                        assert_eq!(key, "limit_posts_in_search");
                    }
                    _ => panic!("expected a SetNumberSetting gesture"),
                }
            }
            _ => unreachable!(),
        }
    }

    /// Committing a typed edit parses it, sends the parsed integer, and clears
    /// the buffer — the next paint owns the displayed text again (module docs:
    /// "the refresh that follows a mutation is what repaints").
    #[test]
    fn a_number_setting_commit_parses_and_clears_the_buffer() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge(
            "activitypub",
            "ActivityPub",
            vec![setting(
                "limit_posts_in_search",
                SETTING_TYPE_NUMBER,
                WireValue::Integer(1000),
            )],
        )];
        set_field(
            &mut app.bridges,
            BridgesField::NumberSetting {
                bridge_id: "activitypub".to_string(),
                key: "limit_posts_in_search".to_string(),
            },
            "  500  ".to_string(),
        );
        let op = apply_local(
            &mut app,
            Action::SetNumberSetting {
                bridge_id: "activitypub".to_string(),
                key: "limit_posts_in_search".to_string(),
            },
        )
        .expect("a valid number commit returns an op");
        match op {
            Op::SetSettings {
                bridge_id,
                settings,
                ..
            } => {
                assert_eq!(bridge_id, "activitypub");
                let WireValue::Map(m) = settings else {
                    panic!("settings is a map")
                };
                assert_eq!(
                    m.get("limit_posts_in_search"),
                    Some(&WireValue::Integer(500))
                );
            }
            _ => panic!("expected Op::SetSettings"),
        }
        assert!(
            !app.bridges.number_setting_inputs.contains_key(&(
                "activitypub".to_string(),
                "limit_posts_in_search".to_string()
            )),
            "the buffer clears so the next repaint reflects the nest's own value"
        );
    }

    /// Activating the input with no local edit re-commits the LIVE value
    /// (idempotent), rather than silently no-op-ing — the same fallback the
    /// paint uses.
    #[test]
    fn a_number_setting_commit_with_no_edit_resends_the_live_value() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge(
            "activitypub",
            "ActivityPub",
            vec![setting(
                "limit_posts_in_search",
                SETTING_TYPE_NUMBER,
                WireValue::Integer(1000),
            )],
        )];
        let op = apply_local(
            &mut app,
            Action::SetNumberSetting {
                bridge_id: "activitypub".to_string(),
                key: "limit_posts_in_search".to_string(),
            },
        )
        .expect("a no-edit commit still resends the live value");
        match op {
            Op::SetSettings { settings, .. } => {
                let WireValue::Map(m) = settings else {
                    panic!("settings is a map")
                };
                assert_eq!(
                    m.get("limit_posts_in_search"),
                    Some(&WireValue::Integer(1000))
                );
            }
            _ => panic!("expected Op::SetSettings"),
        }
    }

    /// An unparseable or negative buffer is refused client-side — no nest
    /// call — and the buffer is left INTACT so the user can see and fix what
    /// they typed (the `add_follow_with_a_blank_id_is_refused` shape).
    #[test]
    fn a_number_setting_commit_with_invalid_text_is_refused() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge(
            "activitypub",
            "ActivityPub",
            vec![setting(
                "limit_posts_in_search",
                SETTING_TYPE_NUMBER,
                WireValue::Integer(1000),
            )],
        )];
        set_field(
            &mut app.bridges,
            BridgesField::NumberSetting {
                bridge_id: "activitypub".to_string(),
                key: "limit_posts_in_search".to_string(),
            },
            "lots".to_string(),
        );
        let op = apply_local(
            &mut app,
            Action::SetNumberSetting {
                bridge_id: "activitypub".to_string(),
                key: "limit_posts_in_search".to_string(),
            },
        );
        assert!(op.is_none(), "unparseable text must not reach the nest");
        assert_eq!(
            app.bridges.number_setting_inputs.get(&(
                "activitypub".to_string(),
                "limit_posts_in_search".to_string()
            )),
            Some(&"lots".to_string()),
            "the bad text stays visible for the user to fix"
        );
    }

    /// Add-follow reads the per-bridge buffers, trims, drops an empty petname,
    /// and clears both buffers so the next visit starts blank.
    #[test]
    fn add_follow_reads_and_clears_the_buffers() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", vec![])];
        set_field(
            &mut app.bridges,
            BridgesField::FollowId {
                bridge_id: "activitypub".to_string(),
            },
            "  @alice@example.social  ".to_string(),
        );
        let op = apply_local(
            &mut app,
            Action::AddFollow {
                bridge_id: "activitypub".to_string(),
            },
        )
        .expect("add-follow returns an op");
        match op {
            Op::AddFollow {
                bridge_id,
                id,
                petname,
                ..
            } => {
                assert_eq!(bridge_id, "activitypub");
                assert_eq!(id, "@alice@example.social");
                assert_eq!(petname, None, "a blank petname is dropped");
            }
            _ => panic!("expected Op::AddFollow"),
        }
        assert!(!app.bridges.follow_id_input.contains_key("activitypub"));
    }

    /// An empty "ID to follow" is refused client-side — no nest call.
    #[test]
    fn add_follow_with_a_blank_id_is_refused() {
        let mut app = bridges_app();
        app.bridges.bridges = vec![linked_bridge("activitypub", "ActivityPub", vec![])];
        let op = apply_local(
            &mut app,
            Action::AddFollow {
                bridge_id: "activitypub".to_string(),
            },
        );
        assert!(op.is_none(), "a blank follow id must not reach the nest");
    }

    /// Folding a `Failed` outcome lands on the page error; a `Loaded` clears it.
    #[test]
    fn the_fold_bridges_a_failure_onto_the_page_error() {
        let mut app = bridges_app();
        apply_outcome(&mut app, Outcome::Failed("nope".to_string()));
        assert_eq!(
            app.errors.get(&Page::Bridges).map(String::as_str),
            Some("nope")
        );

        apply_outcome(
            &mut app,
            Outcome::Loaded {
                bridges: vec![],
                follows: BTreeMap::new(),
            },
        );
        assert!(
            !app.errors.contains_key(&Page::Bridges),
            "a successful load clears it"
        );
    }
}
