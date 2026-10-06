//! The contacts page — roster, knocks, and the Find User form (`contacts.md`).
//!
//! **Where the logic lives** (`contacts.md` § Where logic lives): contacts is
//! deliberately **transport-only** — there is no shared manager (the unifying
//! `ContactsSnapshot` is that doc's open remainder), so the roster + knock
//! state is held here on [`App`], fetched through the thin
//! `fauna_client_contacts::ContactsClient` at login and on every navigation to
//! the tab, exactly as linux holds `state.contacts`/`state.knocks`. The pieces
//! that ARE shared stay shared: the roster filter is
//! `fauna_core::format::contact_matches_filter`, the status badge text is
//! `fauna_core::format::contact_status_label`, lookup classification is
//! `fauna_core::resolve::classify_recipient`, and the knock payload is
//! `fauna_client_core::email::build_knock_payload` sent over
//! `fauna_client_inbox::InboxClient` (`api-layers.md` § Contacts & Knocks —
//! the wire authority).
//!
//! The async split mirrors the conversations page: [`apply_local`] lands the
//! synchronous half and hands back a [`Op`]; the agent's click path
//! **awaits** the op and applies its [`Outcome`] before replying (element
//! reads are single-shot), while the keyboard path spawns it and the outcome
//! comes back through the `UiMessage` channel.
//!
//! **The Address Book segment** (`contacts-view-segment` / `addressbook-item` /
//! `vcard-*`) lives in [`crate::address_book`]: a **separate store** from the
//! social graph here (CardDAV vCards, MDA-sealed), so it keeps its own module,
//! its own transport, and its own rows. This page owns only the segment flag,
//! the ops that feed it, and the paint branch.
//!
//! **Cross-nest Find User + knock** (`contacts.md` § Implementation status
//! today): a typed `bob@other.test` whose domain is not this nest's resolves
//! **directly against that peer** — `fauna_core::resolve::is_foreign_handle_domain`
//! decides, `fauna_provisioning::probe::peer_nest_url` derives the URL, and an
//! anonymous `fauna.actor.by_handle` runs there. The page then carries that URL
//! into `fauna.inbox.send`'s `recipient_nest_url`, so the home nest originates
//! `fauna.federation.inbox.deliver` and the knock lands in the peer's queue.
//!
//! This is the conversations recipient picker's mechanism, not linux's
//! `resolve_nest` → `resolve_handle_on_remote` chain: the nest-proxied
//! `fauna.nest.resolve` refuses loopback / IP-literal / `.local` authorities by
//! design (`discovery_core.rs`), so dialing the typed authority directly is
//! both the richer pattern (priority #4) and the only one a two-nest test
//! topology can witness.

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::NestClient;
use fauna_client_contacts::ContactsClient;
use fauna_client_family::family::FamilyContactRequestInfo;
use fauna_core::identity::ActorKeypair;
use fauna_i18n::strings::{common, contacts as t};
use fauna_protocol::contacts::{ContactItem, KnockItem};
use fauna_protocol::discovery::{ActorByHandleReply, ActorByHandleRequest};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedSender;

use crate::address_book::{AddressbookRow, Segment, VCardRow};
use crate::app::{App, UiMessage};
use crate::element::{Element, Field, Gesture};

/// A gesture on the contacts page. Each maps onto a transport call or a local
/// state change — never a navigation decision of its own.
#[derive(Debug, Clone)]
pub enum Action {
    /// `contact-actor-id-lookup` — classify the typed input
    /// (`classify_recipient`): a 64-hex actor id resolves **offline** (the
    /// echo the knock-send e2e pins); a handle resolves same-nest over
    /// `fauna.actor.by_handle` (async). Malformed input still probes the nest
    /// as a bare handle, mirroring linux's Invalid-branch local resolve, so a
    /// genuinely unknown name reports not-found rather than silence.
    Lookup,
    /// `contacts-add-button` — send the signed knock to the resolved actor
    /// (`build_knock_payload` → `fauna.inbox.send`, same-nest). Async.
    AddContact,
    /// `contact-request-guardian-button` — the supervised ward's in-app ask,
    /// offered ONLY after the nest refused the knock with the typed
    /// guardian-approval error (`family-safety.md` § Child-initiated contact
    /// requests). `fauna.family.contact.request`; async.
    ///
    /// The ask carries **who, never why** — no message text, deliberately: a
    /// ward-authored free-text field is a disclosure surface the guardian's
    /// queue does not need (§ Don't do these), which is why this action has no
    /// payload beyond the resolved peer already on the page.
    RequestContact,
    /// `contact-actor-id-copy-btn` — copy the resolved actor id (OSC 52).
    CopyActorId,
    /// `contacts-accept-button[i]` — `fauna.knocks.accept`. Async; refetches.
    AcceptKnock { peer_id: String },
    /// `contacts-block-button[i]` — `fauna.knocks.block`. Async; refetches.
    BlockKnock { peer_id: String },
    /// `knock-dismiss[i]` — `fauna.knocks.dismiss`. Async; refetches.
    DismissKnock { peer_id: String },
    /// `contact-confirm[i]` — `fauna.contacts.confirm` (promotes an accepted
    /// edge to confirmed). Async; refetches.
    ConfirmContact { peer_id: String },
    /// `contacts-segment-people` — show the social contact graph. Refetches the
    /// roster, so a segment round-trip never lands on a stale list.
    ShowPeople,
    /// `contacts-segment-addressbook` — show the CardDAV Address Book, loading
    /// the actor's books as this gesture's one awaited op.
    ShowAddressBook,
    /// `addressbook-item[i]` — open a book and load its cards. Carries the
    /// book's hex id, since the target is only known at paint time (the
    /// `SelectCalendar` shape the Events page uses for `calendar-item`).
    SelectAddressbook(String),
    /// `vcard-card[i]` — open the `card_detail` sub-page for this card. Purely
    /// local: the row is already unsealed and parsed.
    ///
    /// There is no `CloseCard` twin: leaving `card_detail` is Esc, and this
    /// client's keymap-only escapes clear their page state directly
    /// (`App::handle_key`'s feed/conversations arms do exactly the same) rather
    /// than round-tripping through a gesture no element ever fires.
    OpenCard(String),
    /// Open the card a **`uid_hash`** names — the `SearchNav::Contact` deep
    /// link's destination (`ui/search.md` § User actions), and deliberately NOT
    /// a second spelling of [`Self::OpenCard`].
    ///
    /// Two things separate it from `OpenCard`, and both are why it is its own
    /// action rather than a conversion at the call site: the id is in the
    /// **other id space** (a search row carries the edit-stable `uid_hash`,
    /// never the server-assigned `card_id` this page keys on), and the card need
    /// not be loaded — or even be in the book the user last opened — so this one
    /// takes a network read where `OpenCard` is purely local.
    OpenCardByUid(String),
}

/// A resolved Find User hit — what `contact-actor-id-result` renders and
/// `contacts-add-button` sends to. `actor_id` is the FULL 64-hex id: the
/// cross-app contract is that `get_text("contact-actor-id-result")` returns
/// the raw id (apple once truncated it for display and the knock-send e2e
/// caught it — truncation is paint-only, never the registered text).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindResult {
    pub actor_id: String,
    /// The nest this actor was resolved **on**, when that is not the logged-in
    /// one — i.e. `fauna.inbox.send`'s `recipient_nest_url` for the knock this
    /// result feeds. `None` is the same-nest case (a bare handle, a raw actor
    /// id, or a typed domain that is this nest's own), and the nest then
    /// delivers locally instead of originating
    /// `fauna.federation.inbox.deliver` (`inbox_handlers.rs`).
    ///
    /// It is carried on the *result* rather than recomputed at send time on
    /// purpose: the send must reach the actor the lookup actually found. A
    /// second derivation could drift from the first (a re-resolve, a changed
    /// input box), and the failure mode — knocking a *local* `bob` while the
    /// page shows the foreign one's id — is silent.
    pub nest_url: Option<String>,
}

/// The contacts page's state, hung off [`App`] (`contacts.md` § State & data
/// shape is still a proposed snapshot — until it is ratified the state lives
/// per-app, and tui mirrors linux's fields rather than inventing a shape).
#[derive(Default)]
pub struct ContactsState {
    /// The live WS-RPC channel + the knock-sender identity, installed at the
    /// post-auth hook. `None` pre-auth — every reader degrades gracefully.
    pub nest: Option<Arc<NestClient>>,
    /// The logged-in nest's URL — `build_knock_payload`'s `node_url` (the
    /// sender's origin the recipient can knock back through).
    pub node_url: String,
    /// The actor's signing secret for the knock payload. Held raw like the
    /// feed's copy (the manager-less pages own their signing inputs).
    pub secret: Option<[u8; 32]>,
    /// The account's mail custody (`fauna.state.mail`) — the MSEK the Address
    /// Book half's CardDAV reads unseal under.
    pub mail: Option<Arc<dyn fauna_client_config::MailStore>>,
    /// `fauna.contacts.list` rows, unfiltered (the filter is paint-time).
    pub contacts: Vec<ContactItem>,
    /// `fauna.knocks.list` rows.
    pub knocks: Vec<KnockItem>,
    /// The `contacts-search-field` local roster filter (a LOCAL substring
    /// filter via the shared predicate — never a nest query).
    pub filter: String,
    /// The `contact-actor-id-field` buffer.
    pub find_input: String,
    /// The resolved Find User hit, if any.
    pub find_result: Option<FindResult>,
    /// The `contact-find-error` text, if the last lookup failed.
    pub find_error: Option<String>,
    /// Whether a knock was already sent to the current `find_result` (the
    /// button flips to "Sent" and disables, linux's exact affordance).
    pub knock_sent: bool,
    /// The nest refused the last knock to `find_result` with the typed
    /// guardian-approval error — a supervised ward whose `contact_approval`
    /// knob is on. Reveals `contact-request-guardian-button` in place of a dead
    /// error banner (`family-safety.md` § Child-initiated contact requests).
    ///
    /// Reset by every new lookup, alongside `knock_sent`: the refusal belongs to
    /// the peer it was refused for, and offering the ask against a *different*
    /// resolved peer would ask the guardian about the wrong person.
    pub guardian_refused: bool,
    /// This session just sent the ask, before any status re-read has landed.
    /// `contact-request-pending` renders on this OR on the durable
    /// `status.contact_requests` (via `FamilyState::contact_ask_pending`) — the
    /// local flag is what makes the affordance answer immediately, the durable
    /// list is what makes it survive navigation and a restart.
    pub contact_ask_sent: bool,
    // ── The Address Book segment (`crate::address_book`) ──
    /// Which half of the page is showing (`contacts-view-segment`). **Sticky**
    /// across navigation: a user who left in the Address Book returns to it,
    /// and [`nav_enter_op`] loads whichever half is current.
    pub segment: Segment,
    /// The actor's decoded address books, loaded when the segment opens.
    pub addressbooks: Vec<AddressbookRow>,
    /// The open book's hex id, once one has been picked.
    pub selected_book: Option<String>,
    /// The open book's decoded cards.
    pub cards: Vec<VCardRow>,
    /// The open card's hex id — the `card_detail` sub-page.
    pub open_card: Option<String>,
}

/// Build the page state at the post-auth hook and start the initial roster +
/// knocks fetch (the page hydrates on login; navigating to the tab refetches).
pub fn init(
    nest: Arc<NestClient>,
    node_url: &str,
    secret: [u8; 32],
    mail: Arc<dyn fauna_client_config::MailStore>,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) -> ContactsState {
    let state = ContactsState {
        nest: Some(Arc::clone(&nest)),
        node_url: node_url.to_string(),
        secret: Some(secret),
        mail: Some(mail),
        ..ContactsState::default()
    };
    spawn_refresh(&state, tx, session_generation);
    state
}

/// Fire-and-forget roster + knocks refetch; the result lands through the
/// channel. Used at login, on nav-to-tab, and never from the agent's click
/// path (which awaits its op inline instead).
/// The roster refetch entering this tab implies — the page's leg of the one
/// nav-edge hook (`crate::app::on_nav_enter`). Returns the op; the caller runs
/// it (the agent awaits, the keyboard spawns).
pub fn nav_enter_op(state: &ContactsState) -> Option<Op> {
    let nest = state.nest.clone()?;
    // ONE op per nav edge (the actuation contract): load whichever half the
    // sticky segment is showing, not both.
    match state.segment {
        Segment::People => Some(Op::Refresh { nest }),
        Segment::AddressBook => Some(Op::LoadAddressbooks {
            nest,
            secret: state.secret?,
            mail: state.mail.clone()?,
        }),
    }
}

/// The Address Book re-read a `fauna.addressbook.changed` push asks for
/// (`StaleSurfaces::address_book`) — the books and, when one is open, its cards,
/// so a card another contacts app writes appears on the page the user is
/// looking at.
///
/// `None` unless the Address Book half is the one showing: the People half
/// paints nothing this op reads, and switching to the Address Book re-reads it
/// anyway. The caller page-gates it too — see `StaleSurfaces::address_book`.
pub fn address_book_resync_op(state: &ContactsState) -> Option<Op> {
    if state.segment != Segment::AddressBook {
        return None;
    }
    Some(Op::RefreshAddressBook {
        nest: state.nest.clone()?,
        secret: state.secret?,
        mail: state.mail.clone()?,
        open_book: state.selected_book.clone(),
    })
}

/// Fire-and-forget the roster refetch — the **post-auth** path only, where
/// there is no driver ack to honour and nothing to race. The nav edge goes
/// through [`nav_enter_op`] so it can be awaited.
pub fn spawn_refresh(
    state: &ContactsState,
    tx: &UnboundedSender<UiMessage>,
    session_generation: u64,
) {
    crate::app::spawn_nav_refresh!(nav_enter_op(state), tx, session_generation, Contacts);
}

// ── Field access ──────────────────────────────────────────────────────────────

/// A contacts-page editable field (`crate::contacts`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ContactsField {
    /// `contacts-search-field` — the LOCAL roster filter (`contacts.md`
    /// § Where logic lives: a substring filter over the loaded rows via the
    /// shared predicate, never a nest query). A local buffer; applies at paint.
    Filter,
    /// `contact-actor-id-field` — the Find User input. A local buffer;
    /// `contact-actor-id-lookup` commits it, not each keystroke.
    FindInput,
}

pub fn field(state: &ContactsState, field: &ContactsField) -> String {
    match field {
        ContactsField::Filter => state.filter.clone(),
        ContactsField::FindInput => state.find_input.clone(),
    }
}

/// Both fields are local buffers: the roster filter applies at paint time
/// (shared predicate over the loaded rows — never a nest query), and the find
/// input commits on `contact-actor-id-lookup`, not per keystroke.
pub fn set_field(state: &mut ContactsState, field: ContactsField, value: String) {
    match field {
        ContactsField::Filter => state.filter = value,
        ContactsField::FindInput => state.find_input = value,
    }
}

// ── Gesture dispatch ────────────────────────────────────────────────────────

/// Apply a contacts gesture's local half and hand back its network half, if
/// any — the same split the feed and conversations pages use, and for the same
/// reason (the agent awaits; the keyboard spawns).
impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback
    /// arm, so a new contacts gesture must answer the offline question.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            // Find User. A raw actor id short-circuits with no nest hop at all
            // (see [`apply_local`]); a handle probes `fauna.actor.by_handle`,
            // which is a `Read` and so never desensitizes anyway. Naming the
            // read is still the honest answer — the class decides, not us.
            Action::Lookup => Some("fauna.actor.by_handle"),
            // The knock ride on `fauna.inbox.send` — queued, not online-only:
            // a knock composed offline drains on reconnect.
            Action::AddContact => Some("fauna.inbox.send"),
            // The ward's ask. Queued like the knock it stands in for: an ask
            // composed offline is a replayable intent, and the nest's own
            // dedup makes a replayed one a no-op.
            Action::RequestContact => Some("fauna.family.contact.request"),
            Action::AcceptKnock { .. } => Some("fauna.knocks.accept"),
            Action::BlockKnock { .. } => Some("fauna.knocks.block"),
            Action::DismissKnock { .. } => Some("fauna.knocks.dismiss"),
            Action::ConfirmContact { .. } => Some("fauna.contacts.confirm"),
            // Local: a clipboard write, a segment/detail switch, a book or
            // card selection. Each may *trigger* a refetch, but the refetch is
            // a `Read` that fails harmlessly offline — and desensitizing
            // navigation would strand the user on whatever page they were on
            // when the connection dropped.
            Action::CopyActorId
            | Action::ShowPeople
            | Action::ShowAddressBook
            | Action::SelectAddressbook(_)
            | Action::OpenCard(_)
            | Action::OpenCardByUid(_) => None,
        }
    }
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    let st = &mut app.contacts;
    match action {
        Action::Lookup => {
            // A new lookup resets the previous result/error; the sent-flag
            // belongs to the result it was sent for.
            st.find_result = None;
            st.find_error = None;
            st.knock_sent = false;
            // The refusal and the ask both belong to the peer they were made
            // for — carrying either across a new lookup would offer to ask the
            // guardian about somebody the ward did not name.
            st.guardian_refused = false;
            st.contact_ask_sent = false;
            let nest = st.nest.clone()?;
            match fauna_core::resolve::classify_recipient(&st.find_input) {
                // A raw actor id is addressable directly — resolve OFFLINE
                // (no nest hop), the short-circuit the knock-send e2e pins.
                fauna_core::resolve::RecipientInput::ActorId(id) => {
                    // A bare id names no domain, so there is nothing to route
                    // cross-nest on — this knock goes to the home nest.
                    st.find_result = Some(FindResult {
                        actor_id: id,
                        nest_url: None,
                    });
                    None
                }
                // A typed `user@domain` resolves same-nest first with the domain
                // threaded (the nest disambiguates multi-domain handles); if the
                // reply proves the domain foreign, the op falls through to the
                // peer itself (module docs).
                fauna_core::resolve::RecipientInput::Handle { user, domain } => Some(Op::Lookup {
                    nest,
                    handle: user,
                    domain: Some(domain),
                }),
                // Anything else still probes the nest as a bare handle —
                // linux's Invalid-branch local resolve. An unknown name comes
                // back not-found; a plain `alice` resolves.
                fauna_core::resolve::RecipientInput::Invalid => Some(Op::Lookup {
                    nest,
                    handle: st.find_input.trim().to_string(),
                    domain: None,
                }),
            }
        }
        Action::AddContact => {
            let nest = st.nest.clone()?;
            let secret = st.secret?;
            let found = st.find_result.as_ref()?;
            let recipient = found.actor_id.clone();
            // Where the lookup found them — carried, never re-derived (see
            // `FindResult::nest_url`).
            let recipient_nest_url = found.nest_url.clone();
            if st.knock_sent {
                return None; // the button is disabled; belt-and-suspenders
            }
            Some(Op::SendKnock {
                nest,
                node_url: st.node_url.clone(),
                secret,
                recipient,
                recipient_nest_url,
            })
        }
        Action::RequestContact => {
            let nest = st.nest.clone()?;
            let peer = st.find_result.as_ref()?.actor_id.clone();
            // Belt-and-suspenders beside the disabled button: a re-ask while one
            // is pending is a quiet nest-side no-op anyway (the primary key
            // dedups and it never re-rings the guardian), but not issuing it is
            // cheaper and keeps the affordance honest about its own state.
            if st.contact_ask_sent {
                return None;
            }
            Some(Op::RequestContact { nest, peer })
        }
        Action::CopyActorId => {
            if let Some(r) = &st.find_result {
                crate::wizard::copy_to_clipboard(&r.actor_id);
            }
            None
        }
        Action::AcceptKnock { peer_id } => knock_op(st, KnockKind::Accept, peer_id),
        Action::BlockKnock { peer_id } => knock_op(st, KnockKind::Block, peer_id),
        Action::DismissKnock { peer_id } => knock_op(st, KnockKind::Dismiss, peer_id),
        Action::ConfirmContact { peer_id } => knock_op(st, KnockKind::Confirm, peer_id),
        Action::ShowPeople => {
            st.segment = Segment::People;
            // Leaving the Address Book closes its detail, so coming back lands
            // on the card list rather than a card the user has long forgotten.
            st.open_card = None;
            Some(Op::Refresh {
                nest: st.nest.clone()?,
            })
        }
        Action::ShowAddressBook => {
            st.segment = Segment::AddressBook;
            st.open_card = None;
            Some(Op::LoadAddressbooks {
                nest: st.nest.clone()?,
                secret: st.secret?,
                mail: st.mail.clone()?,
            })
        }
        Action::SelectAddressbook(id) => {
            st.selected_book = Some(id.clone());
            st.open_card = None;
            // Drop the previous book's cards NOW, not when the load lands:
            // leaving them painted under the newly-picked book would let a read
            // in the gap select a card belonging to the book just left.
            st.cards.clear();
            Some(Op::LoadCards {
                nest: st.nest.clone()?,
                secret: st.secret?,
                mail: st.mail.clone()?,
                book_id: id,
            })
        }
        Action::OpenCard(id) => {
            st.open_card = Some(id);
            None
        }
        Action::OpenCardByUid(uid_hash) => {
            // Show the Address Book half straight away — the read below is a
            // round trip, and landing on the People segment first would flash
            // the wrong half of the page. The card list is cleared for the same
            // reason `SelectAddressbook` clears it: whatever is painted under a
            // pending jump belongs to a book the user is leaving.
            st.segment = Segment::AddressBook;
            st.open_card = None;
            st.selected_book = None;
            st.cards.clear();
            Some(Op::LocateCard {
                nest: st.nest.clone()?,
                secret: st.secret?,
                mail: st.mail.clone()?,
                uid_hash,
            })
        }
    }
}

fn knock_op(state: &ContactsState, kind: KnockKind, peer_id: String) -> Option<Op> {
    Some(Op::KnockAction {
        nest: state.nest.clone()?,
        kind,
        peer_id,
    })
}

/// Which `ContactsClient` mutation a [`Op::KnockAction`] runs.
#[derive(Debug, Clone, Copy)]
pub enum KnockKind {
    Accept,
    Block,
    Dismiss,
    Confirm,
}

/// The network half of a contacts gesture — owns only `Arc`s + owned data, so
/// it can be awaited on the agent's path or spawned on the keyboard's.
pub enum Op {
    /// Fetch roster + knocks (login, nav-to-tab, and after every mutation).
    Refresh { nest: Arc<NestClient> },
    /// `fauna.actor.by_handle` (the Find User handle branch) — same-nest first,
    /// then the peer directly when `domain` proves foreign.
    Lookup {
        nest: Arc<NestClient>,
        handle: String,
        domain: Option<String>,
    },
    /// `build_knock_payload` → `fauna.inbox.send`. `recipient_nest_url` is the
    /// lookup's [`FindResult::nest_url`]: `None` delivers locally, `Some`
    /// makes the home nest originate `fauna.federation.inbox.deliver`.
    SendKnock {
        nest: Arc<NestClient>,
        node_url: String,
        secret: [u8; 32],
        recipient: String,
        recipient_nest_url: Option<String>,
    },
    /// `fauna.family.contact.request` — the supervised ward's ask, offered only
    /// after `SendKnock` came back refused by the guardian gate.
    RequestContact { nest: Arc<NestClient>, peer: String },
    /// One of the four peer-edge mutations, then a refetch (the reply is an
    /// empty ack, so the fresh lists ARE the observable effect).
    KnockAction {
        nest: Arc<NestClient>,
        kind: KnockKind,
        peer_id: String,
    },
    /// `fauna.bridges.list_addressbooks` + unseal — the Address Book picker.
    LoadAddressbooks {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        mail: Arc<dyn fauna_client_config::MailStore>,
    },
    /// `fauna.bridges.query_cards` + unseal/parse — one book's card list.
    LoadCards {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        mail: Arc<dyn fauna_client_config::MailStore>,
        book_id: String,
    },
    /// The push-driven re-read ([`address_book_resync_op`]): the books, then
    /// `open_book`'s cards when one is open.
    RefreshAddressBook {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        mail: Arc<dyn fauna_client_config::MailStore>,
        open_book: Option<String>,
    },
    /// The `SearchNav::Contact` deep link's read: books + the book holding
    /// `uid_hash` + that card's `card_id`, in one walk
    /// (`crate::address_book::locate_card`).
    LocateCard {
        nest: Arc<NestClient>,
        secret: [u8; 32],
        mail: Arc<dyn fauna_client_config::MailStore>,
        uid_hash: String,
    },
}

/// What an op resolved to — applied to [`App`] by [`apply_outcome`]
/// (synchronously on the agent's path; via the channel on the keyboard's).
#[derive(Debug)]
pub enum Outcome {
    /// Fresh roster + knocks (Refresh and every KnockAction end here).
    Loaded {
        contacts: Vec<ContactItem>,
        knocks: Vec<KnockItem>,
    },
    /// A transport failure loading or mutating — lands on `error-message`.
    Failed(String),
    /// The Find User handle branch resolved. `nest_url` is `Some` only when the
    /// actor was found on a **peer** nest (see [`FindResult::nest_url`]).
    Resolved {
        actor_id: String,
        nest_url: Option<String>,
    },
    /// The Find User handle branch found nothing (`contact-find-error`).
    NotFound,
    /// The knock was accepted by the nest (the button flips to "Sent").
    KnockSent,
    /// The knock send failed — lands on `error-message` (the knock-send e2e
    /// asserts no error banner on success, so failures must be loud).
    KnockFailed(String),
    /// The nest refused the knock because this account's new contacts need
    /// guardian approval. Its own outcome rather than a `KnockFailed` string:
    /// the refusal is *typed* precisely so the app can offer
    /// `contact-request-guardian-button` in place of a dead banner
    /// (`family-safety.md` § Child-initiated contact requests).
    KnockRefusedByGuardian,
    /// `fauna.family.contact.request` landed — the guardian has been rung.
    ///
    /// Carries the ward's re-read `status.contact_requests`, because the ask's
    /// own reply is a bare ack: without the re-read the durable pending state
    /// would not land until the *next* family-page visit or a restart, so the
    /// affordance would silently revert to a Knock button the moment the ward
    /// looked the peer up again. (An e2e reload caught exactly that.) Empty on a
    /// failed re-read — the local flag is the fallback, so a landed ask is never
    /// rendered as un-asked.
    ContactRequested {
        requests: Vec<FamilyContactRequestInfo>,
    },
    /// The ask itself failed (cap reached, peer already blocked, knob off — all
    /// typed refusals the ward should read verbatim, not a generic banner).
    ContactRequestFailed(String),
    /// The actor's address books, decoded (`addressbook-item` rows).
    AddressbooksLoaded(Vec<AddressbookRow>),
    /// One book's cards, decoded. Carries the `book_id` it was fetched for so a
    /// late reply for a book the user has since left cannot overwrite the list
    /// they are actually looking at.
    CardsLoaded {
        book_id: String,
        cards: Vec<VCardRow>,
    },
    /// The `SearchNav::Contact` deep link's walk finished — books, and the
    /// holding book + its cards + the target `card_id` when one still holds it.
    CardLocated(crate::address_book::LocatedCard),
    /// [`Op::RefreshAddressBook`] landed: the books, and the cards of the book
    /// that was open when it was issued (`None` when none was).
    AddressBookRefreshed {
        books: Vec<AddressbookRow>,
        open: Option<(String, Vec<VCardRow>)>,
    },
}

/// How a knock send ended — the one classification the contacts page and the
/// profile page (`profile-request-contact-button`) both render from, so the
/// guardian gate is told apart from every other failure in exactly one place.
#[derive(Debug)]
pub enum KnockSend {
    Sent,
    /// The nest's typed guardian-approval refusal
    /// (`RpcError::is_guardian_approval_required`) — the one failure that
    /// reveals `contact-request-guardian-button`.
    RefusedByGuardian,
    Failed(String),
}

/// `build_knock_payload` → `fauna.inbox.send`. `recipient_nest_url` `None`
/// delivers on this nest; `Some(peer)` makes the home nest originate
/// `fauna.federation.inbox.deliver` to that peer (`inbox_handlers.rs`;
/// `federation.md` § the inbox send).
pub(crate) async fn send_knock(
    nest: Arc<NestClient>,
    node_url: &str,
    secret: [u8; 32],
    recipient: String,
    recipient_nest_url: Option<String>,
) -> KnockSend {
    let kp = ActorKeypair::from_secret(secret);
    let recipient_bytes = match fauna_core::hex32::decode(&recipient) {
        Ok(b) => b,
        Err(e) => return KnockSend::Failed(format!("recipient id: {e}")),
    };
    let payload =
        match fauna_client_core::email::build_knock_payload(&kp, &recipient_bytes, node_url) {
            Ok(p) => p,
            Err(e) => return KnockSend::Failed(format!("knock payload: {e}")),
        };
    match fauna_client_inbox::InboxClient::new(nest)
        .send(recipient, recipient_nest_url, payload)
        .await
    {
        Ok(_) => KnockSend::Sent,
        // The guardian gate is separated from every other failure here, on the
        // shared predicate rather than a per-app string match — the namespace
        // naming *which* handler refused is the nest's business, not a page's.
        Err(fauna_client::NestClientError::Rpc(err)) if err.is_guardian_approval_required() => {
            KnockSend::RefusedByGuardian
        }
        Err(e) => KnockSend::Failed(format!("inbox send: {e}")),
    }
}

/// `fauna.family.contact.request` — the supervised ward's ask, offered only
/// after a knock came back [`KnockSend::RefusedByGuardian`]. On success it
/// re-reads the ward's own asks in the same op, so what paints is what the NEST
/// holds rather than this session's memory of having asked. A failed re-read is
/// not a failed ask — the guardian has been rung — so it degrades to an empty
/// list and the caller's local flag carries the render.
pub(crate) async fn ask_guardian(
    nest: Arc<NestClient>,
    peer: &str,
) -> Result<Vec<FamilyContactRequestInfo>, String> {
    let peer_bytes = fauna_core::hex32::decode(peer).map_err(|e| format!("peer id: {e}"))?;
    let client = fauna_client_family::FamilyClient::new(nest);
    client
        .contact_request(peer_bytes.to_vec())
        .await
        .map_err(|e| e.to_string())?;
    Ok(client
        .status()
        .await
        .map(|s| s.contact_requests)
        .unwrap_or_default())
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Refresh { nest } => refresh(nest).await,
            Op::Lookup {
                nest,
                handle,
                domain,
            } => {
                let req = ActorByHandleRequest {
                    handle: handle.clone(),
                    domain: domain.clone(),
                    extra: Default::default(),
                };
                let reply: Result<ActorByHandleReply, _> =
                    nest.request("fauna.actor.by_handle", req).await;
                // The same-nest reply's `domain` IS this nest's handle domain,
                // which is what makes the typed one judgeable; a failed probe
                // volunteers none, and the shared rule reads that as "go ask the
                // peer" rather than silently resolving `bob@other.test` to a
                // local `bob`.
                let home_domain = reply.as_ref().ok().map(|r| r.domain.as_str());
                if !fauna_core::resolve::is_foreign_handle_domain(domain.as_deref(), home_domain) {
                    return match reply {
                        Ok(r) => Outcome::Resolved {
                            actor_id: r.actor_id,
                            nest_url: None,
                        },
                        // An unknown handle is a rejection, not a transport
                        // fault — both surface as not-found here (the page's
                        // find-error), matching linux's HANDLE_NOT_FOUND arm.
                        Err(_) => Outcome::NotFound,
                    };
                }
                resolve_on_peer(handle, domain.expect("foreign implies a typed domain")).await
            }
            Op::SendKnock {
                nest,
                node_url,
                secret,
                recipient,
                recipient_nest_url,
            } => match send_knock(nest, &node_url, secret, recipient, recipient_nest_url).await {
                KnockSend::Sent => Outcome::KnockSent,
                KnockSend::RefusedByGuardian => Outcome::KnockRefusedByGuardian,
                KnockSend::Failed(e) => Outcome::KnockFailed(e),
            },
            Op::RequestContact { nest, peer } => match ask_guardian(nest, &peer).await {
                Ok(requests) => Outcome::ContactRequested { requests },
                Err(e) => Outcome::ContactRequestFailed(e),
            },
            Op::KnockAction {
                nest,
                kind,
                peer_id,
            } => {
                let client = ContactsClient::new(Arc::clone(&nest));
                let result = match kind {
                    KnockKind::Accept => client.knocks_accept(peer_id).await.map(|_| ()),
                    KnockKind::Block => client.knocks_block(peer_id).await.map(|_| ()),
                    KnockKind::Dismiss => client.knocks_dismiss(peer_id).await.map(|_| ()),
                    KnockKind::Confirm => client.contacts_confirm(peer_id).await.map(|_| ()),
                };
                match result {
                    // The mutation reply is an empty ack — refetch so the rows
                    // the driver counts are the nest's own view.
                    Ok(()) => refresh(nest).await,
                    Err(e) => Outcome::Failed(e.to_string()),
                }
            }
            Op::LoadAddressbooks { nest, secret, mail } => {
                match crate::address_book::load_addressbooks(nest, secret, mail).await {
                    Ok(books) => Outcome::AddressbooksLoaded(books),
                    Err(e) => Outcome::Failed(e),
                }
            }
            Op::LoadCards {
                nest,
                secret,
                mail,
                book_id,
            } => match crate::address_book::load_cards(nest, secret, mail, book_id.clone()).await {
                Ok(cards) => Outcome::CardsLoaded { book_id, cards },
                Err(e) => Outcome::Failed(e),
            },
            Op::LocateCard {
                nest,
                secret,
                mail,
                uid_hash,
            } => match crate::address_book::locate_card(nest, secret, mail, &uid_hash).await {
                Ok(located) => Outcome::CardLocated(located),
                Err(e) => Outcome::Failed(e),
            },
            Op::RefreshAddressBook {
                nest,
                secret,
                mail,
                open_book,
            } => {
                let books = match crate::address_book::load_addressbooks(
                    Arc::clone(&nest),
                    secret,
                    Arc::clone(&mail),
                )
                .await
                {
                    Ok(books) => books,
                    Err(e) => return Outcome::Failed(e),
                };
                let open = match open_book {
                    Some(book_id) => {
                        match crate::address_book::load_cards(nest, secret, mail, book_id.clone())
                            .await
                        {
                            Ok(cards) => Some((book_id, cards)),
                            Err(e) => return Outcome::Failed(e),
                        }
                    }
                    None => None,
                };
                Outcome::AddressBookRefreshed { books, open }
            }
        }
    }
}

/// Resolve `handle` on the **peer** nest that serves `domain` — the cross-nest
/// half of Find User, reached once
/// [`fauna_core::resolve::is_foreign_handle_domain`] has ruled the typed domain
/// foreign.
///
/// An anonymous connection straight to the typed authority, exactly as the
/// conversations recipient picker's `actor_by_handle_remote` does
/// (`fauna_client_conversations`): `peer_nest_url` derives the base URL, and
/// `fauna.actor.by_handle` runs there pre-identity. The home nest is not in the
/// path — deliberately. The nest-proxied `fauna.nest.resolve` kind, which
/// linux's older `resolve_nest` → `resolve_handle_on_remote` chain goes
/// through, **refuses** loopback, IP-literal and `.local` authorities by design
/// (`bins/fauna-nest/src/discovery_core.rs`), so it can neither reach a peer on
/// a LAN address nor be witnessed by a two-nest test topology. Dialing the
/// authority directly is both the richer existing pattern (priority #4) and the
/// one that actually works.
///
/// The returned [`Outcome::Resolved`] carries the peer URL, which the knock
/// send then hands to `fauna.inbox.send` as `recipient_nest_url`.
///
/// Every failure — the peer being unreachable, refusing, or not knowing the
/// handle — collapses to [`Outcome::NotFound`], matching the same-nest arm: the
/// page has one `contact-find-error` and the user's next move ("check the
/// address") is the same either way.
async fn resolve_on_peer(handle: String, domain: String) -> Outcome {
    use fauna_anon_client::AnonymousNestClient;

    let Some(peer_url) = fauna_provisioning::probe::peer_nest_url(Some(domain)) else {
        return Outcome::NotFound;
    };
    let Ok(client) = AnonymousNestClient::connect(&peer_url).await else {
        return Outcome::NotFound;
    };
    let reply: Result<ActorByHandleReply, _> = client
        .request(
            "fauna.actor.by_handle",
            ActorByHandleRequest {
                handle,
                // `None`, not the typed domain: we are already talking to the
                // nest that serves it, so the multi-domain qualifier has
                // nothing left to disambiguate and the peer reports its own
                // canonical domain. Same call shape as `actor_by_handle_remote`.
                domain: None,
                extra: Default::default(),
            },
        )
        .await;
    match reply {
        Ok(r) => Outcome::Resolved {
            actor_id: r.actor_id,
            nest_url: Some(peer_url),
        },
        Err(_) => Outcome::NotFound,
    }
}

async fn refresh(nest: Arc<NestClient>) -> Outcome {
    let client = ContactsClient::new(nest);
    let contacts = match client.contacts_list().await {
        Ok(r) => r.contacts,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    let knocks = match client.knocks_list().await {
        Ok(r) => r.knocks,
        Err(e) => return Outcome::Failed(e.to_string()),
    };
    Outcome::Loaded { contacts, knocks }
}

/// Fold an op's result back into the page. One function for both dispatch
/// paths, so they cannot disagree about what an outcome means.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    let st = &mut app.contacts;
    match outcome {
        Outcome::Loaded { contacts, knocks } => {
            st.contacts = contacts;
            st.knocks = knocks;
            app.errors.remove(&crate::pages::Page::Contacts);
        }
        Outcome::Failed(e) => {
            app.errors.insert(crate::pages::Page::Contacts, e);
        }
        Outcome::Resolved { actor_id, nest_url } => {
            st.find_result = Some(FindResult { actor_id, nest_url });
            st.find_error = None;
        }
        Outcome::NotFound => {
            st.find_result = None;
            st.find_error = Some(t::HANDLE_NOT_FOUND.to_string());
        }
        Outcome::KnockSent => {
            st.knock_sent = true;
        }
        Outcome::KnockFailed(e) => {
            app.errors.insert(crate::pages::Page::Contacts, e);
        }
        Outcome::KnockRefusedByGuardian => {
            st.guardian_refused = true;
            // Still a real error on `error-message` — the send genuinely did not
            // happen, and convention 2 says a page states its failures. What
            // changes is that it is no longer a DEAD end: the ask button paints
            // beside it.
            app.errors.insert(
                crate::pages::Page::Contacts,
                t::GUARDIAN_APPROVAL_REQUIRED.to_string(),
            );
        }
        Outcome::ContactRequested { requests } => {
            st.contact_ask_sent = true;
            // The nest's own list replaces ours — this is what makes the pending
            // state durable *within* the session, not merely across a restart.
            // An empty list here means the re-read failed, and dropping the
            // existing one on that would be strictly worse than keeping it.
            if !requests.is_empty() {
                app.family.own_contact_requests = requests;
            }
            app.errors.remove(&crate::pages::Page::Contacts);
        }
        Outcome::ContactRequestFailed(e) => {
            // The ask's own typed refusals (cap reached, peer blocked, knob off)
            // are the ward's to read verbatim — this is the one path where the
            // guardian gate's own error would be actively misleading.
            app.errors.insert(crate::pages::Page::Contacts, e);
        }
        Outcome::AddressbooksLoaded(books) => {
            st.addressbooks = books;
            app.errors.remove(&crate::pages::Page::Contacts);
        }
        Outcome::AddressBookRefreshed { books, open } => {
            st.addressbooks = books;
            // The same guard as `CardsLoaded`: the refresh is spawned, so the
            // user may have opened another book while it was in flight, and a
            // reply for the book they left must not overwrite the one they are
            // on. An open card the write deleted degrades to the list on paint.
            if let Some((book_id, cards)) = open
                && st.selected_book.as_deref() == Some(book_id.as_str())
            {
                st.cards = cards;
            }
            app.errors.remove(&crate::pages::Page::Contacts);
        }
        Outcome::CardsLoaded { book_id, cards } => {
            // Only apply if this reply is for the book still open — a slow read
            // for a book the user has since left must not repopulate the list
            // under them (the awaited-op path cannot race, but the keyboard
            // path spawns, so both orders are reachable).
            if st.selected_book.as_deref() == Some(book_id.as_str()) {
                st.cards = cards;
            }
            app.errors.remove(&crate::pages::Page::Contacts);
        }
        Outcome::CardLocated(located) => {
            // The picker's rows land either way — the walk read them, and a
            // deep link arrives on a page the user may never have opened, so
            // leaving `addressbooks` empty would strand them on a blank picker
            // the moment they close the card.
            st.addressbooks = located.books;
            match located.open {
                Some((book_id, cards, card_id)) => {
                    st.selected_book = Some(book_id);
                    st.cards = cards;
                    st.open_card = Some(card_id);
                    app.errors.remove(&crate::pages::Page::Contacts);
                }
                // No book holds that `uid_hash` any more: the card was deleted
                // between being indexed and being clicked. Say so on
                // `error-message` — the same DROPPED outcome a query-time
                // resolve gives, except this one has a user waiting on it, and
                // a silently-unchanged page would read as a dead row.
                None => {
                    app.errors.insert(
                        crate::pages::Page::Contacts,
                        t::address_book::CARD_NOT_FOUND.to_string(),
                    );
                }
            }
        }
    }
}

// ── Elements ────────────────────────────────────────────────────────────────

/// The row's names through the one shared resolver
/// (`fauna_core::format::peer_display_label`, value-formatting.md § Peer
/// display label): the viewer's own nickname for this person, else the
/// enriched handle, else the canonical `short_id`. `public` is the name a
/// nickname replaced — the row's `contact-public-name` line.
fn contact_display(app: &App, c: &ContactItem) -> fauna_core::format::PeerLabel {
    match overlay_projection(app) {
        Some(p) => p.peer_label(None, c.handle.as_deref(), &c.peer_id),
        None => fauna_core::format::peer_display_label(None, None, c.handle.as_deref(), &c.peer_id),
    }
}

/// The private contact overlay projection (`contacts.md` § The private
/// overlay), held by the conversations manager and fed from the account store
/// — `None` before a session exists (no overlay reads as none).
pub(crate) fn overlay_projection(
    app: &App,
) -> Option<Arc<fauna_conversations::contacts::ContactsCache>> {
    app.conversations.manager.as_ref().map(|m| m.contacts())
}

/// The contacts page as one ordered element list (paint = registry = focus
/// ring). Flat indexed ids for the knock/contact rows, like every list on this
/// client; the roster applies the shared filter predicate at paint time, so a
/// narrowed roster registers fewer `contact-name` rows — exactly what the
/// cross-app filter e2e counts.
/// Whether this contact row is a person the owner still owes a review verdict
/// on (`identity-succession.md` § Propagation → *MLS groups*).
///
/// The `peer_id` is the actor-id hex the row is keyed by, so the join is a
/// decode plus the one shared predicate — never a second definition of "is this
/// person flagged". A `peer_id` that does not decode is not under review: the
/// roster's people are MLS group members, all of whom have an actor id.
fn contact_under_review(app: &App, c: &ContactItem) -> bool {
    fauna_core::data::is_under_review_hex(&app.member_reviews, &c.peer_id)
}

pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.contacts;
    let mut out = vec![
        Element::label(ids::PAGE_HEADING, t::TITLE),
        // The page container marker (ui.yaml `contacts-view`) — same
        // empty-text convention as `conversations-view`.
        Element::label(ids::CONTACTS_VIEW, " "),
        // The `contacts-view-segment` toggle. Painted in BOTH segments: it is
        // the only way back out of the Address Book half, and clicking the
        // already-active side is a harmless refetch rather than a dead control.
        Element::gesture_button(
            ids::CONTACTS_SEGMENT_PEOPLE,
            t::TITLE,
            true,
            Gesture::Contacts(Action::ShowPeople),
        ),
        Element::gesture_button(
            ids::CONTACTS_SEGMENT_ADDRESSBOOK,
            t::address_book::TITLE,
            true,
            Gesture::Contacts(Action::ShowAddressBook),
        ),
    ];

    // The Address Book is a separate store with its own master-detail, so the
    // segment swaps the page BODY (`contacts.md` § Layout & flow). The social
    // half below is simply not painted while it is open — on a terminal that
    // unregisters the ids, which is what makes `is_visible` answer false.
    if st.segment == Segment::AddressBook {
        crate::address_book::render(st, &mut out);
        return out;
    }

    out.push(
        Element::input(
            ids::CONTACTS_SEARCH_FIELD,
            st.filter.clone(),
            Field::Contacts(ContactsField::Filter),
        )
        .labelled(t::FILTER_PLACEHOLDER),
    );
    // The Find User form (`add-contact-form` component).
    out.push(
        Element::input(
            ids::CONTACT_ACTOR_ID_FIELD,
            st.find_input.clone(),
            Field::Contacts(ContactsField::FindInput),
        )
        .labelled(t::HANDLE_OR_ACTOR_ID),
    );
    out.push(Element::gesture_button(
        ids::CONTACT_ACTOR_ID_LOOKUP,
        t::find_user::FIND,
        true,
        Gesture::Contacts(Action::Lookup),
    ));
    if let Some(r) = &st.find_result {
        // The FULL id — truncation would be paint-only; the registered text is
        // the cross-app `get_text` contract (module docs on `FindResult`).
        out.push(Element::label(
            ids::CONTACT_ACTOR_ID_RESULT,
            r.actor_id.clone(),
        ));
        out.push(Element::gesture_button(
            ids::CONTACTS_ADD_BUTTON,
            if st.knock_sent { t::SENT } else { t::KNOCK },
            !st.knock_sent,
            Gesture::Contacts(Action::AddContact),
        ));
        out.push(Element::gesture_button(
            ids::CONTACT_ACTOR_ID_COPY_BTN,
            common::COPY,
            true,
            Gesture::Contacts(Action::CopyActorId),
        ));
        // The ward's in-app ask (`family-safety.md` § Child-initiated contact
        // requests). Offered only after the nest actually refused — the knob
        // may be off, in which case the ward contacts freely and an ask would be
        // refused as moot — and rendered as the *pending* state instead once one
        // is outstanding.
        //
        // `pending` reads the durable `status.contact_requests` FIRST and the
        // just-asked flag second: the durable list is what survives navigation
        // and a restart, and it is also what makes the state honest on a fresh
        // session where this page never saw the refusal at all.
        let pending = app.family.contact_ask_pending(&r.actor_id) || st.contact_ask_sent;
        if pending {
            out.push(Element::label(
                ids::CONTACT_REQUEST_PENDING,
                t::CONTACT_REQUEST_PENDING.to_string(),
            ));
        } else if st.guardian_refused {
            out.push(Element::gesture_button(
                ids::CONTACT_REQUEST_GUARDIAN_BUTTON,
                t::ASK_GUARDIAN,
                true,
                Gesture::Contacts(Action::RequestContact),
            ));
        }
    }
    if let Some(e) = &st.find_error {
        out.push(Element::label(ids::CONTACT_FIND_ERROR, e.clone()));
    }

    // Pending knocks (`knock-request-item` component), one flat-indexed block
    // per knock in reply order.
    for k in &st.knocks {
        out.push(Element::label(ids::KNOCK_CARD, k.summary.clone()));
        // Through the one resolver with no handle: the viewer's nickname for
        // this sender when they gave one, else the shared short id as on every
        // app (`contacts.md` § Where logic lives → Knock sender display, § The
        // private overlay); the full id rides on the gestures.
        let nickname = overlay_projection(app).and_then(|p| p.nickname(&k.sender));
        out.push(Element::label(
            ids::KNOCK_SENDER,
            fauna_core::format::peer_display_label(nickname.as_deref(), None, None, &k.sender)
                .primary,
        ));
        out.push(Element::gesture_button(
            ids::CONTACTS_ACCEPT_BUTTON,
            common::ACCEPT,
            true,
            Gesture::Contacts(Action::AcceptKnock {
                peer_id: k.sender.clone(),
            }),
        ));
        out.push(Element::gesture_button(
            ids::CONTACTS_BLOCK_BUTTON,
            common::BLOCK,
            true,
            Gesture::Contacts(Action::BlockKnock {
                peer_id: k.sender.clone(),
            }),
        ));
        out.push(Element::gesture_button(
            ids::KNOCK_DISMISS,
            common::DISMISS,
            true,
            Gesture::Contacts(Action::DismissKnock {
                peer_id: k.sender.clone(),
            }),
        ));
    }

    // The roster (`contact-list-item` component), filtered through the shared
    // predicate — a filtered-out contact registers nothing, so the driver's
    // `count("contact-name")` reads the narrowed roster (the filter e2e).
    let overlays = overlay_projection(app);
    let labels_of = |c: &ContactItem| {
        overlays
            .as_ref()
            .map(|p| p.labels(&c.peer_id))
            .unwrap_or_default()
    };
    let matched: Vec<&ContactItem> = st
        .contacts
        .iter()
        .filter(|c| {
            fauna_core::format::contact_matches_filter(
                &st.filter,
                c.handle.as_deref(),
                c.domain.as_deref(),
                &c.peer_id,
                overlays
                    .as_ref()
                    .and_then(|p| p.nickname(&c.peer_id))
                    .as_deref(),
                &labels_of(c),
            )
        })
        .collect();

    // `contacts-no-matches` — a NON-EMPTY roster narrowed to zero rows must say
    // so, distinguishably from the true-empty roster (`contacts.md` § Errors &
    // edge cases case (c), the ratified half of that section; cases (a)/(b)
    // remain TBD and are deliberately NOT invented here). Web's is the richer
    // condition and the one this mirrors — linux keys on "query non-empty"
    // alone, which cannot tell an empty account from a narrowed one.
    if !st.filter.trim().is_empty() && !st.contacts.is_empty() && matched.is_empty() {
        out.push(Element::label(
            ids::CONTACTS_NO_MATCHES,
            t::NO_MATCHING_CONTACTS,
        ));
    }

    for (i, c) in matched.into_iter().enumerate() {
        out.push(Element::label(ids::CONTACT_ROW, " "));
        // The post-succession review badge — the third rendering of the one flag
        // (`identity-succession.md` § Propagation → *MLS groups*: group member
        // lists, contacts where applicable, the permanent view).
        //
        // ⚠ **A badge only, with no Keep/Remove pair, and that is the ruling —
        // not an omission.** The decision belongs where removal already lives,
        // which for a person under review is the group member chip; a contacts
        // row has no eviction affordance to join, and minting one here would be
        // the second removal mechanism § Propagation forbids.
        //
        // ⚠ Scoped `.within(ids::CONTACT_ROW, i)` because it renders on flagged
        // rows only — flat, its index would count flagged contacts while every
        // sibling element in this loop counts contacts, and the two would name
        // different people.
        //
        // ⚠ This badge is deliberately NOT the surface the feature relies on: a
        // group member frequently is not a contact at all, and the entry that
        // matters most — an identity a thief seated in a group — is precisely
        // the one that never was.
        if contact_under_review(app, c) {
            out.push(
                Element::label(ids::CONTACT_UNATTESTED_MARK, t::UNATTESTED_MARK)
                    .within(ids::CONTACT_ROW, i),
            );
        }
        // The row is addressable by the peer's actor-id hex and tapping it opens
        // that actor's Profile detail view — the cross-app contract the e2e
        // `open_contact_profile(hex)` drives (linux sets the `ListBoxRow`
        // widget_name to the peer_id; `profile.md` § Relationship to Contacts).
        // It is a literal, data-keyed element id like `dns-provider-row[<id>]`,
        // not an invented shim.
        let name = contact_display(app, c);
        out.push(Element::gesture_button(
            c.peer_id.clone(),
            name.primary.clone(),
            true,
            Gesture::OpenProfile(Some(c.peer_id.clone())),
        ));
        out.push(Element::label(ids::CONTACT_NAME, name.primary.clone()));
        // The private overlay's two row lines (contacts.md § The private
        // overlay), scoped to the row because each renders on some rows only:
        // the public name a nickname replaced (the "never hide the public
        // identity" guard), and the viewer's labels on one line.
        if let Some(public) = name.public {
            out.push(Element::label(ids::CONTACT_PUBLIC_NAME, public).within(ids::CONTACT_ROW, i));
        }
        let labels = labels_of(c);
        if !labels.is_empty() {
            out.push(
                Element::label(ids::CONTACT_LABELS, labels.join(", ")).within(ids::CONTACT_ROW, i),
            );
        }
        out.push(Element::label(
            ids::CONTACT_STATUS,
            crate::wizard::localized(&fauna_core::format::contact_status_label(&c.status)),
        ));
        // Rendered only on an `accepted` row (user ruling IN-PERSON 2026-08-15,
        // contacts.md § Layout & flow region 2): confirm promotes an accepted
        // edge, so a confirmed/blocked row gets no affordance rather than a
        // safe-but-dead button.
        if fauna_core::data::ContactStatus::from_wire(&c.status)
            == Some(fauna_core::data::ContactStatus::Accepted)
        {
            out.push(Element::gesture_button(
                ids::CONTACT_CONFIRM,
                common::CONFIRM,
                true,
                Gesture::Contacts(Action::ConfirmContact {
                    peer_id: c.peer_id.clone(),
                }),
            ));
        }
    }
    out
}

// ── e2e state serializer ──────────────────────────────────────────────────────

/// The `data.contacts` half of `GET /app/state` — ui.yaml's
/// `contacts.state_fields`, exactly (`peer_id` / `status` / `handle`).
pub fn state_json(state: &ContactsState) -> Value {
    Value::Array(
        state
            .contacts
            .iter()
            .map(|c| {
                json!({
                    "peer_id": c.peer_id,
                    "status": c.status,
                    "handle": c.handle,
                })
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::authed_app;
    use crate::pages::Page;

    fn a_contact(peer_id: &str, handle: Option<&str>, status: &str) -> ContactItem {
        ContactItem {
            peer_id: peer_id.to_string(),
            status: status.to_string(),
            accepted_at: None,
            created_at: 0,
            handle: handle.map(str::to_string),
            domain: handle.map(|_| "self-nest.test".to_string()),
            extra: Default::default(),
        }
    }

    fn contacts_app() -> crate::app::App {
        let mut app = authed_app();
        app.page = Page::Contacts;
        // The Address Book ops read the account's mail custody since
        // `fauna.state.mail` went plane-only; without a store they build no op.
        app.contacts.mail = Some(Arc::new(
            fauna_client_config::test_helpers::FakeMailStore::empty(),
        ));
        app
    }

    /// One `status.contact_requests` row for a peer whose id is `byte` repeated
    /// — the ward's own pending ask (`family-safety.md` § Child-initiated
    /// contact requests → *Ward transparency*).
    fn an_ask(byte: u8) -> FamilyContactRequestInfo {
        FamilyContactRequestInfo {
            peer_actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            peer_handle: "alice".into(),
            created_at: 0,
            extra: Default::default(),
        }
    }

    fn ids(app: &crate::app::App) -> Vec<String> {
        elements(app).into_iter().map(|e| e.id).collect()
    }

    fn count_id(app: &crate::app::App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    /// `contact-confirm` renders on `accepted` rows only (user ruling IN-PERSON
    /// 2026-08-15; contacts.md § Layout & flow region 2 — the confirm action is
    /// for accepted edges). A confirmed row must carry NO confirm affordance:
    /// the nest guard makes a stray click safe, but a dead button is still the
    /// wrong surface, and four of seven apps already gated it this way.
    #[test]
    fn contact_confirm_renders_on_accepted_rows_only() {
        let mut app = contacts_app();
        app.contacts.contacts = vec![
            a_contact(&"aa".repeat(32), Some("ada"), "confirmed"),
            a_contact(&"bb".repeat(32), Some("bo"), "accepted"),
            a_contact(&"cc".repeat(32), Some("cy"), "blocked"),
        ];
        assert_eq!(
            count_id(&app, "contact-confirm"),
            1,
            "exactly the accepted row offers confirm"
        );
        assert_eq!(
            count_id(&app, "contact-row"),
            3,
            "the gate hides the affordance, never the row"
        );
    }

    /// The post-succession badge rides the same one flag the group member chips
    /// do, keyed on the row's own `peer_id` — so it appears on the flagged
    /// contact and on no other row, and it is scoped to that row rather than
    /// pushed flat (it renders on flagged contacts only, so a flat index would
    /// count flagged contacts while every sibling counts contacts).
    ///
    /// ⚠ The fixture deliberately puts the flagged contact SECOND: a flat render
    /// would still put the badge at index 0, and every count in this test would
    /// pass while the badge named the wrong person.
    #[test]
    fn the_review_badge_marks_the_flagged_contact_and_is_scoped_to_its_row() {
        let mut app = contacts_app();
        let flagged = fauna_core::identity::ActorId([4u8; 32]);
        let flagged_hex = fauna_core::hex32::encode(&flagged.0);
        app.contacts.contacts = vec![
            a_contact(
                &fauna_core::hex32::encode(&[5u8; 32]),
                Some("ada"),
                "confirmed",
            ),
            a_contact(&flagged_hex, Some("bo"), "confirmed"),
        ];

        assert_eq!(
            count_id(&app, "contact-unattested-mark"),
            0,
            "an un-succeeded account flags nobody"
        );

        app.member_reviews = vec![fauna_core::data::MemberReview {
            person: flagged,
            reasons: vec![fauna_core::data::MemberUnattestedReason::CompromiseWindow],
        }];
        let marks: Vec<Vec<(String, usize)>> = elements(&app)
            .into_iter()
            .filter(|e| e.id == "contact-unattested-mark")
            .map(|e| {
                e.path
                    .iter()
                    .map(|(container, index)| (container.clone(), *index))
                    .collect()
            })
            .collect();
        assert_eq!(
            marks,
            vec![vec![("contact-row".to_string(), 1)]],
            "exactly the second row — the flagged one — carries the badge"
        );

        // ⚠ A badge and nothing else: the Keep/Remove decision lives on the
        // group member chip, where removal already exists. A Keep button here
        // would be the second removal mechanism § Propagation forbids.
        let painted = ids(&app);
        assert!(
            !painted.iter().any(|id| id.contains("keep")),
            "the contacts row offers no verdict affordance: {painted:?}"
        );
    }

    /// `contacts-no-matches` must separate THREE states that all render zero
    /// rows, which is the entire reason the element exists (`contacts.md`
    /// § Errors & edge cases case (c); linux shipped the mirror-image bug of
    /// showing "No contacts yet." over a narrowed roster).
    #[test]
    fn no_matches_shows_only_for_a_narrowed_non_empty_roster() {
        let mut app = contacts_app();
        let shown = |a: &crate::app::App| count_id(a, "contacts-no-matches") == 1;

        // (1) Empty account, no query — the TRUE-empty state, not a narrowing.
        assert!(
            !shown(&app),
            "an empty roster with no query must not claim 'no matches'"
        );

        // (2) Empty account WITH a query — still true-empty; nothing was narrowed
        // away, so this must stay silent (the condition linux's query-only key
        // gets wrong).
        app.contacts.filter = "zzq".into();
        assert!(!shown(&app), "an empty roster must not report a narrowing");

        // (3) Non-empty roster narrowed to zero — the real case.
        app.contacts.contacts = vec![a_contact("ab", Some("ada"), "confirmed")];
        assert_eq!(
            count_id(&app, "contact-row"),
            0,
            "the filter must hide the row"
        );
        assert!(shown(&app), "a roster narrowed to zero must say so");

        // (4) Query that still matches — must NOT show while rows remain.
        app.contacts.filter = "ada".into();
        assert_eq!(count_id(&app, "contact-row"), 1);
        assert!(
            !shown(&app),
            "must not show while the filtered roster still has rows"
        );

        // (5) Cleared — restored, message gone.
        app.contacts.filter = String::new();
        assert_eq!(count_id(&app, "contact-row"), 1);
        assert!(!shown(&app));
    }

    /// The `contacts-view-segment` toggle is page chrome, not a mode-local
    /// control: `switch_to_address_book()` waits for it on arrival, and
    /// `switch_to_people()` needs it to still be there afterwards, so a shell
    /// that painted it in only one half would strand the user in the other.
    #[test]
    fn both_segment_buttons_register_in_both_segments() {
        let mut app = contacts_app();
        for seg in [Segment::People, Segment::AddressBook] {
            app.contacts.segment = seg;
            let painted = ids(&app);
            assert!(
                painted.contains(&"contacts-segment-people".to_string()),
                "contacts-segment-people missing in {seg:?}"
            );
            assert!(
                painted.contains(&"contacts-segment-addressbook".to_string()),
                "contacts-segment-addressbook missing in {seg:?}"
            );
        }
    }

    /// The segment swaps the page BODY. On a terminal "hidden" IS "not painted"
    /// — an unregistered id makes `is_visible` answer false and `wait_for`
    /// block, the equivalence a GTK `set_visible(false)` buys the other shells.
    /// Both directions matter: leaking the roster into the Address Book would
    /// let a stray `contact-row` read succeed against a page the user is not on.
    #[test]
    fn the_segment_swaps_which_half_of_the_page_registers() {
        let mut app = contacts_app();
        app.contacts.contacts = vec![a_contact("ab", Some("ada"), "confirmed")];
        app.contacts.addressbooks = vec![crate::address_book::AddressbookRow {
            id: "aa".into(),
            name: "Contacts".into(),
            card_count: 1,
        }];

        app.contacts.segment = Segment::People;
        let people = ids(&app);
        assert!(people.contains(&"contacts-search-field".to_string()));
        assert!(people.contains(&"contact-row".to_string()));
        assert!(
            !people.contains(&"addressbook-item".to_string()),
            "the Address Book picker must not register while People is showing"
        );

        app.contacts.segment = Segment::AddressBook;
        let book = ids(&app);
        assert!(book.contains(&"addressbook-item".to_string()));
        assert!(
            !book.contains(&"contacts-search-field".to_string())
                && !book.contains(&"contact-row".to_string()),
            "the social roster must not register while the Address Book is showing"
        );
    }

    /// Entering the tab must load whatever the sticky segment is showing, in
    /// ONE op (the actuation contract) — an Address-Book user returning to the
    /// page would otherwise refetch the roster and see a stale, empty picker.
    #[test]
    fn nav_enter_loads_the_half_the_sticky_segment_is_showing() {
        let mut app = contacts_app();
        // Both arms need the transport inputs to build an op at all (the dummy
        // nest shape this file already uses); the secret is what LoadAddressbooks
        // additionally requires, so seed both or the test proves nothing.
        app.contacts.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        ));
        app.contacts.secret = Some([7u8; 32]);

        app.contacts.segment = Segment::People;
        assert!(matches!(
            nav_enter_op(&app.contacts),
            Some(Op::Refresh { .. })
        ));

        app.contacts.segment = Segment::AddressBook;
        assert!(matches!(
            nav_enter_op(&app.contacts),
            Some(Op::LoadAddressbooks { .. })
        ));
    }

    /// A `fauna.addressbook.changed` push re-reads the books and the OPEN book's
    /// cards — and only while the Address Book half is the one showing, since
    /// the People half paints nothing that read returns.
    #[test]
    fn an_address_book_push_rereads_the_open_book_only_while_that_half_shows() {
        let mut app = contacts_app();
        app.contacts.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        ));
        app.contacts.secret = Some([7u8; 32]);
        app.contacts.selected_book = Some("aa".into());

        app.contacts.segment = Segment::People;
        assert!(
            address_book_resync_op(&app.contacts).is_none(),
            "the People half must not re-read the Address Book"
        );

        app.contacts.segment = Segment::AddressBook;
        assert!(matches!(
            address_book_resync_op(&app.contacts),
            Some(Op::RefreshAddressBook { open_book: Some(ref b), .. }) if b == "aa"
        ));

        app.contacts.selected_book = None;
        assert!(matches!(
            address_book_resync_op(&app.contacts),
            Some(Op::RefreshAddressBook {
                open_book: None,
                ..
            })
        ));
    }

    /// The refresh lands the books whatever happened meanwhile, but its cards
    /// only onto the book still open — the refresh is spawned, so the user may
    /// have picked another book while it was in flight.
    #[test]
    fn an_address_book_refresh_lands_its_cards_only_on_the_book_still_open() {
        let mut app = contacts_app();
        let card = |id: &str, name: &str| crate::address_book::VCardRow {
            id: id.into(),
            formatted_name: name.into(),
            ..Default::default()
        };
        let book = |id: &str| crate::address_book::AddressbookRow {
            id: id.into(),
            name: "Contacts".into(),
            card_count: 2,
        };
        app.contacts.selected_book = Some("bb".into());
        app.contacts.cards = vec![card("c0", "Kept")];

        apply_outcome(
            &mut app,
            Outcome::AddressBookRefreshed {
                books: vec![book("aa"), book("bb")],
                open: Some(("aa".into(), vec![card("c1", "Ada")])),
            },
        );
        assert_eq!(app.contacts.addressbooks.len(), 2, "the books always land");
        assert_eq!(
            app.contacts.cards[0].formatted_name, "Kept",
            "cards for book aa must not land while book bb is open"
        );

        apply_outcome(
            &mut app,
            Outcome::AddressBookRefreshed {
                books: vec![book("bb")],
                open: Some(("bb".into(), vec![card("c1", "Ada"), card("c2", "Grace")])),
            },
        );
        let names: Vec<_> = app
            .contacts
            .cards
            .iter()
            .map(|c| c.formatted_name.as_str())
            .collect();
        assert_eq!(
            names,
            ["Ada", "Grace"],
            "the open book's fresh cards replace its list"
        );
    }

    /// A late `CardsLoaded` for a book the user has already left must not
    /// repopulate the list under them. The agent's awaited path cannot race,
    /// but the keyboard path spawns, so both orders are reachable.
    #[test]
    fn a_stale_cards_reply_for_another_book_is_dropped() {
        let mut app = contacts_app();
        app.contacts.selected_book = Some("bb".into());
        apply_outcome(
            &mut app,
            Outcome::CardsLoaded {
                book_id: "aa".into(),
                cards: vec![crate::address_book::VCardRow {
                    id: "c1".into(),
                    formatted_name: "Ada".into(),
                    ..Default::default()
                }],
            },
        );
        assert!(
            app.contacts.cards.is_empty(),
            "cards for book aa must not land while book bb is open"
        );

        apply_outcome(
            &mut app,
            Outcome::CardsLoaded {
                book_id: "bb".into(),
                cards: vec![crate::address_book::VCardRow {
                    id: "c2".into(),
                    formatted_name: "Bob".into(),
                    ..Default::default()
                }],
            },
        );
        assert_eq!(app.contacts.cards.len(), 1);
    }

    /// A resolved `SearchNav::Contact` deep link lands the WHOLE Address Book
    /// half from one read: the picker's books, the holding book selected, its
    /// cards, and the target card open — keyed on the `card_id` the locate
    /// returned, never on the `uid_hash` the search row carried.
    #[test]
    fn a_located_card_opens_its_detail_and_fills_the_book_around_it() {
        let mut app = contacts_app();
        // Drive the real sequence — the ACTION owns the navigation (segment
        // swap), the OUTCOME owns the data, exactly like `SelectAddressbook` /
        // `CardsLoaded`. Applying the outcome alone would paint nothing and
        // prove nothing.
        app.contacts.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        ));
        app.contacts.secret = Some([7u8; 32]);
        apply_local(&mut app, Action::OpenCardByUid("abcd".into()));
        apply_outcome(
            &mut app,
            Outcome::CardLocated(crate::address_book::LocatedCard {
                books: vec![
                    crate::address_book::AddressbookRow {
                        id: "b1".into(),
                        name: "Work".into(),
                        card_count: 1,
                    },
                    crate::address_book::AddressbookRow {
                        id: "b2".into(),
                        name: "Family".into(),
                        card_count: 1,
                    },
                ],
                open: Some((
                    "b2".into(),
                    vec![crate::address_book::VCardRow {
                        id: "c2".into(),
                        formatted_name: "Ada".into(),
                        ..Default::default()
                    }],
                    "c2".into(),
                )),
            }),
        );
        assert_eq!(app.contacts.selected_book.as_deref(), Some("b2"));
        assert_eq!(app.contacts.open_card.as_deref(), Some("c2"));
        assert_eq!(app.contacts.cards.len(), 1);
        assert_eq!(
            app.contacts.addressbooks.len(),
            2,
            "the picker must be populated too — closing the card cannot strand \
             the user on an empty book list they never loaded"
        );
        assert!(
            !app.errors.contains_key(&Page::Contacts),
            "a successful jump clears the page error"
        );

        // ...and the card detail actually paints, which is the property a
        // matching `open_card` only *implies*: the pane renders by finding that
        // id among the loaded cards, so a mismatched id space would leave this
        // silently empty (the whole reason the locate exists).
        assert_eq!(count_id(&app, "vcard-detail-fn"), 1);
    }

    /// The card was deleted between being indexed and being clicked: say so on
    /// `error-message` rather than leaving the page silently unchanged, which
    /// would read as a dead row.
    #[test]
    fn a_deep_link_to_a_deleted_card_reports_it_instead_of_going_quiet() {
        let mut app = contacts_app();
        apply_outcome(
            &mut app,
            Outcome::CardLocated(crate::address_book::LocatedCard {
                books: vec![crate::address_book::AddressbookRow {
                    id: "b1".into(),
                    name: "Work".into(),
                    card_count: 0,
                }],
                open: None,
            }),
        );
        assert_eq!(
            app.errors.get(&Page::Contacts).map(String::as_str),
            Some(t::address_book::CARD_NOT_FOUND),
        );
        assert!(app.contacts.open_card.is_none());
        assert_eq!(
            app.contacts.addressbooks.len(),
            1,
            "the books still land, so the page it fails onto is a real picker"
        );
    }

    /// The activation itself: `OpenCardByUid` shows the Address Book half
    /// immediately and clears whatever book was under it, so the pending jump
    /// never paints another book's cards — and it asks for the locate rather
    /// than trying to open the `uid_hash` as if it were a `card_id`.
    #[test]
    fn open_card_by_uid_switches_to_the_address_book_and_asks_for_the_locate() {
        let mut app = contacts_app();
        app.contacts.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        ));
        app.contacts.secret = Some([7u8; 32]);
        app.contacts.segment = Segment::People;
        app.contacts.selected_book = Some("b1".into());
        app.contacts.cards = vec![crate::address_book::VCardRow {
            id: "c1".into(),
            formatted_name: "Stale".into(),
            ..Default::default()
        }];
        app.contacts.open_card = Some("c1".into());

        let op = apply_local(&mut app, Action::OpenCardByUid("abcd".into()));
        assert!(matches!(op, Some(Op::LocateCard { .. })));
        assert_eq!(app.contacts.segment, Segment::AddressBook);
        assert!(app.contacts.selected_book.is_none());
        assert!(app.contacts.cards.is_empty());
        assert!(
            app.contacts.open_card.is_none(),
            "the previous book's open card must not stay painted under a pending jump"
        );
    }

    /// The page's fixed chrome registers even on an empty state — the driver's
    /// `is_visible` probes (`contacts-view`, the find form, the search field)
    /// must resolve on a fresh account with no rows.
    #[test]
    fn the_page_chrome_registers_on_an_empty_roster() {
        let app = contacts_app();
        let got = ids(&app);
        for required in [
            "page-heading",
            "contacts-view",
            "contacts-search-field",
            "contact-actor-id-field",
            "contact-actor-id-lookup",
        ] {
            assert!(
                got.contains(&required.to_string()),
                "must register {required}"
            );
        }
        assert_eq!(count_id(&app, "contact-name"), 0);
        assert_eq!(count_id(&app, "knock-card"), 0);
    }

    /// `knock-sender` names the sender by the shared `short_id` — the one text
    /// all 7 apps render for it (`contacts.md` § Where logic lives → Knock
    /// sender display) — while the summary stays on `knock-card`. tui used to
    /// register the full 64-hex id here, and linux the summary.
    #[test]
    fn a_knock_names_its_sender_by_the_shared_short_id() {
        let mut app = contacts_app();
        let sender = "ab".repeat(32);
        app.contacts.knocks = vec![KnockItem {
            id: 1,
            sender: sender.clone(),
            sender_node: "self-nest.test".to_string(),
            summary: "hello from ab".to_string(),
            created_at: 0,
            extra: Default::default(),
        }];
        let els = elements(&app);
        let texts = |id: &str| -> Vec<String> {
            els.iter()
                .filter(|e| e.id == id)
                .map(|e| e.text.clone())
                .collect()
        };
        assert_eq!(
            texts("knock-sender"),
            vec![fauna_core::format::short_id(&sender)],
            "knock-sender must be the shared short id — not the full hex, not the summary"
        );
        assert_eq!(texts("knock-card"), vec!["hello from ab".to_string()]);
    }

    /// The roster filter is the shared predicate applied at PAINT time: a
    /// narrowed query registers fewer `contact-name` rows (the count the
    /// cross-app filter e2e reads), and clearing restores them.
    #[test]
    fn the_roster_filter_narrows_painted_rows_via_the_shared_predicate() {
        let mut app = contacts_app();
        app.contacts.contacts = vec![
            a_contact("aa".repeat(32).as_str(), Some("rosterbob"), "accepted"),
            a_contact("bb".repeat(32).as_str(), Some("carol"), "confirmed"),
        ];
        assert_eq!(count_id(&app, "contact-name"), 2);

        app.contacts.filter = "rosterbob".to_string();
        assert_eq!(
            count_id(&app, "contact-name"),
            1,
            "handle match keeps one row"
        );

        app.contacts.filter = "zzqqxxnomatch".to_string();
        assert_eq!(
            count_id(&app, "contact-name"),
            0,
            "no match hides every row"
        );

        app.contacts.filter.clear();
        assert_eq!(
            count_id(&app, "contact-name"),
            2,
            "clearing restores the roster"
        );
    }

    /// A 64-hex actor id resolves OFFLINE (no op returned, the result set
    /// synchronously) — the short-circuit the knock-send e2e's single-shot
    /// `contact-actor-id-result` read depends on. The registered text is the
    /// FULL id. Anything else returns a nest-probe op instead.
    #[test]
    fn a_raw_actor_id_resolves_offline_and_registers_the_full_id() {
        let mut app = contacts_app();
        // No nest on the test state — install a dummy so apply_local can build ops.
        app.contacts.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        ));
        let id = "ab".repeat(32);
        app.contacts.find_input = id.clone();

        let op = apply_local(&mut app, Action::Lookup);
        assert!(op.is_none(), "a raw actor id must not hit the nest");
        assert_eq!(
            elements(&app)
                .into_iter()
                .find(|e| e.id == "contact-actor-id-result")
                .expect("result registers")
                .text,
            id,
            "the registered text is the FULL id (truncation is paint-only)"
        );
        assert_eq!(count_id(&app, "contacts-add-button"), 1);

        // A non-hex input probes the nest (handle branch) instead.
        app.contacts.find_input = "nonexistent_actor_00000000".to_string();
        assert!(apply_local(&mut app, Action::Lookup).is_some());
    }

    /// **The cross-nest knock join** (`contacts.md` § Implementation status
    /// today). A find result that resolved on a *peer*
    /// nest must carry that peer's URL into the knock op as
    /// `recipient_nest_url` — that field is the entire difference between the
    /// nest local-delivering the knock and it originating
    /// `fauna.federation.inbox.deliver` to the peer
    /// (`bins/fauna-nest/src/inbox_handlers.rs`).
    ///
    /// Asserted at the `apply_local` seam rather than over the wire because the
    /// wire half already has its own proof
    /// (`conformance_federation_channel::inbox_send_cross_nest_stores_a_knock_for_a_stranger`):
    /// what is unproven, and silent when wrong, is the *page* dropping the URL
    /// on the floor between the lookup and the send. A regression here knocks a
    /// same-named LOCAL actor while the page displays the foreign one's id.
    #[test]
    fn a_peer_resolved_find_result_carries_its_nest_url_into_the_knock() {
        let mut app = contacts_app();
        app.contacts.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:9".to_string(),
            fauna_core::identity::ActorKeypair::from_secret([7u8; 32]),
        ));
        app.contacts.secret = Some([7u8; 32]);
        app.contacts.node_url = "https://home.test".to_string();

        // Same-nest first: a result with no peer URL sends `None`, the shape
        // that makes the nest deliver locally.
        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: "ab".repeat(32),
                nest_url: None,
            },
        );
        match apply_local(&mut app, Action::AddContact) {
            Some(Op::SendKnock {
                recipient_nest_url, ..
            }) => assert_eq!(
                recipient_nest_url, None,
                "a same-nest result must not federate"
            ),
            other => panic!("expected a SendKnock op, got {:?}", other.is_some()),
        }

        // Now the cross-nest result. A fresh lookup clears `knock_sent`, which
        // the belt-and-suspenders guard in `AddContact` would otherwise trip.
        app.contacts.knock_sent = false;
        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: "cd".repeat(32),
                nest_url: Some("https://peer.test:8443".to_string()),
            },
        );
        match apply_local(&mut app, Action::AddContact) {
            Some(Op::SendKnock {
                recipient,
                recipient_nest_url,
                ..
            }) => {
                assert_eq!(
                    recipient_nest_url.as_deref(),
                    Some("https://peer.test:8443"),
                    "the peer the lookup found is the peer the knock is sent to"
                );
                assert_eq!(
                    recipient,
                    "cd".repeat(32),
                    "and it is that peer's actor, not the earlier same-nest one"
                );
            }
            other => panic!("expected a SendKnock op, got {:?}", other.is_some()),
        }
    }

    /// Outcome folding: not-found surfaces `contact-find-error`; a sent knock
    /// flips the button to a disabled "Sent" (linux's exact affordance).
    #[test]
    fn outcomes_fold_back_onto_the_page() {
        let mut app = contacts_app();
        apply_outcome(&mut app, Outcome::NotFound);
        assert_eq!(count_id(&app, "contact-find-error"), 1);

        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: "cd".repeat(32),
                nest_url: None,
            },
        );
        assert_eq!(
            count_id(&app, "contact-find-error"),
            0,
            "a new result clears the error"
        );
        apply_outcome(&mut app, Outcome::KnockSent);
        let add = elements(&app)
            .into_iter()
            .find(|e| e.id == "contacts-add-button")
            .expect("button still registers");
        assert!(!add.enabled, "a sent knock disables the button");
        assert_eq!(add.text, t::SENT);
    }

    /// `family-safety.md` § Child-initiated contact requests: a supervised
    /// ward's refused send is not a dead end — the typed refusal reveals
    /// `contact-request-guardian-button`, and sending the ask swaps it for
    /// `contact-request-pending`.
    ///
    /// The affordance is offered ONLY on the typed refusal, never on any other
    /// send failure: a network drop that painted "ask your guardian" would
    /// teach an *unsupervised* user that their account is supervised.
    #[test]
    fn a_guardian_refused_knock_offers_the_ask_and_then_shows_it_pending() {
        let mut app = contacts_app();
        let peer = "cd".repeat(32);
        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: peer.clone(),
                nest_url: None,
            },
        );
        // A clean resolve offers neither.
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 0);
        assert_eq!(count_id(&app, "contact-request-pending"), 0);

        // An ordinary failure stays an ordinary failure.
        apply_outcome(&mut app, Outcome::KnockFailed("inbox send: boom".into()));
        assert_eq!(
            count_id(&app, "contact-request-guardian-button"),
            0,
            "a transport failure must not imply supervision",
        );

        apply_outcome(&mut app, Outcome::KnockRefusedByGuardian);
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 1);
        assert_eq!(count_id(&app, "contact-request-pending"), 0);
        // Still a real error: the send did not happen (convention 2).
        assert_eq!(
            app.errors.get(&Page::Contacts).map(String::as_str),
            Some(t::GUARDIAN_APPROVAL_REQUIRED),
        );

        apply_outcome(
            &mut app,
            Outcome::ContactRequested {
                requests: vec![an_ask(0xcd)],
            },
        );
        assert_eq!(
            count_id(&app, "contact-request-guardian-button"),
            0,
            "the ask is sent — offering it again would re-ask",
        );
        assert_eq!(count_id(&app, "contact-request-pending"), 1);
        assert!(
            !app.errors.contains_key(&Page::Contacts),
            "a landed ask clears the refusal banner",
        );
        // The op's re-read is what makes the state durable IN-SESSION: without
        // it the pending render would revert to a Knock button the moment the
        // ward looked the same peer up again (an e2e reload caught exactly
        // that — the flag cleared, and nothing had refreshed the list).
        assert!(
            app.family.contact_ask_pending(&peer),
            "the ask must land in the ward's own status list, not just a flag",
        );
    }

    /// A failed status re-read is NOT a failed ask — the guardian has been rung
    /// either way — so an empty list must not wipe what the client already
    /// holds, and the local flag still carries the render.
    #[test]
    fn a_failed_status_reread_after_an_ask_keeps_the_pending_render() {
        let mut app = contacts_app();
        let peer = "cd".repeat(32);
        app.family.own_contact_requests = vec![an_ask(0xab)];
        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: peer.clone(),
                nest_url: None,
            },
        );
        apply_outcome(&mut app, Outcome::KnockRefusedByGuardian);
        apply_outcome(&mut app, Outcome::ContactRequested { requests: vec![] });

        assert_eq!(
            count_id(&app, "contact-request-pending"),
            1,
            "the local flag carries the render when the re-read failed",
        );
        assert_eq!(
            app.family.own_contact_requests.len(),
            1,
            "an empty re-read must not wipe the asks already held",
        );
    }

    /// The pending state is DURABLE, not a session flag: it renders from the
    /// ward's own `status.contact_requests` (§ Child-initiated contact requests
    /// → *Ward transparency*), so a ward who asked yesterday and reopened the
    /// app still sees "asked — waiting" instead of a Knock button that would
    /// only be refused again.
    #[test]
    fn a_pending_ask_from_the_status_read_renders_without_this_session_asking() {
        let mut app = contacts_app();
        let peer = "cd".repeat(32);
        app.family.own_contact_requests = vec![an_ask(0xcd)];
        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: peer.clone(),
                nest_url: None,
            },
        );
        assert_eq!(count_id(&app, "contact-request-pending"), 1);
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 0);

        // …and it is keyed on the PEER, not on "some ask exists": resolving a
        // different actor must not inherit the first one's pending state.
        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: "ab".repeat(32),
                nest_url: None,
            },
        );
        assert_eq!(
            count_id(&app, "contact-request-pending"),
            0,
            "a different peer must not inherit the ask",
        );
    }

    /// A new lookup drops both the refusal and the just-asked flag: they belong
    /// to the peer they were made for, and carrying them across would offer to
    /// ask the guardian about somebody the ward never named.
    #[test]
    fn a_new_lookup_clears_the_refusal_and_the_pending_flag() {
        let mut app = contacts_app();
        apply_outcome(
            &mut app,
            Outcome::Resolved {
                actor_id: "cd".repeat(32),
                nest_url: None,
            },
        );
        apply_outcome(&mut app, Outcome::KnockRefusedByGuardian);
        apply_outcome(
            &mut app,
            Outcome::ContactRequested {
                requests: vec![an_ask(0xcd)],
            },
        );
        assert!(app.contacts.guardian_refused && app.contacts.contact_ask_sent);

        app.contacts.find_input = "ab".repeat(32);
        apply_local(&mut app, Action::Lookup);
        assert!(!app.contacts.guardian_refused);
        assert!(!app.contacts.contact_ask_sent);
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 0);
        assert_eq!(
            count_id(&app, "contact-request-pending"),
            0,
            "the OTHER peer has no ask — the durable list must not leak across",
        );
    }

    /// The private overlay on the roster (contacts.md § The private overlay):
    /// a nickname is the row's name with the public name on its own line, the
    /// labels ride on one line, a row without an overlay is unchanged but for
    /// the resolver's short-id fallback, and typing a label narrows the roster.
    #[test]
    fn the_roster_paints_the_private_overlay_and_filters_on_its_labels() {
        use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp, fold_label};
        let mut app = contacts_app();
        let mum = "aa".repeat(32);
        let bare = "bb".repeat(32);
        app.contacts.contacts = vec![
            a_contact(&mum, Some("ada"), "confirmed"),
            a_contact(&bare, None, "confirmed"),
        ];
        let manager = fauna_conversations::ConversationsManager::new();
        let reg = |v: &str| Register {
            stamp: Stamp::new(1, [1; 32]),
            value: Some(v.to_string()),
        };
        let generation = manager.register_contact_overlays(None);
        manager.apply_contact_overlays(
            generation,
            [(
                mum.clone(),
                ContactOverlay {
                    nickname: reg("Mum"),
                    labels: ["Family", "Book club"]
                        .iter()
                        .map(|l| (fold_label(l), reg(l)))
                        .collect(),
                    ..Default::default()
                },
            )]
            .into_iter()
            .collect(),
        );
        app.conversations.manager = Some(manager);

        let texts = |app: &crate::app::App, id: &str| -> Vec<String> {
            elements(app)
                .into_iter()
                .filter(|e| e.id == id)
                .map(|e| e.text)
                .collect()
        };
        assert_eq!(
            texts(&app, "contact-name"),
            vec!["Mum".to_string(), fauna_core::format::short_id(&bare)]
        );
        assert_eq!(texts(&app, "contact-public-name"), vec!["ada"]);
        assert_eq!(texts(&app, "contact-labels"), vec!["Book club, Family"]);

        app.contacts.filter = "family".into();
        assert_eq!(texts(&app, "contact-name"), vec!["Mum"]);
        app.contacts.filter = "mum".into();
        assert_eq!(count_id(&app, "contact-row"), 1);
    }
}
