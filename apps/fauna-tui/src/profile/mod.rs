//! The profile page — M5 slice 2 (`docs/goal/ui/profile.md`; ui.yaml `profile`).
//!
//! The **canonical per-user surface**: one detail view of any actor (`is_self`
//! branches the whole page). SELF shows the identity header + the text-only
//! **edit form** (publish/edit path); OTHER shows the header + the secondary
//! relationship actions (follow / start-DM / block) and the Tiers-tab
//! **offers browse** (`profile.md` § Layout & flow).
//!
//! **All logic is shared Rust** (`profile.md` § Where logic lives) — the page is
//! glue over `fauna_client_profile::{ProfileClient, build_edited_profile,
//! decode_profile_display}`, `fauna_client_subscriptions::SubscriptionsClient`,
//! `fauna_client_contacts::ContactsClient` (block/unblock), and the shared
//! `fauna_core::format` derivations (`offer_status`, `contact_toggle_block_label`,
//! `contact_row_blocks_actor`). There is **no client-side profile cache**: like
//! the feed (`feed.md` observer-free rule), every render reads live truth, so a
//! (re)open re-fetches.
//!
//! The async split mirrors the contacts/notifications pages: [`apply_local`]
//! lands the synchronous half and hands back a [`Op`]; the agent's click
//! path **awaits** the op and applies its [`Outcome`] before replying (element
//! reads are single-shot), while the keyboard path spawns it and the outcome
//! comes back through the `UiMessage` channel. The open-time fetches
//! ([`spawn_open_refresh`]) always ride the channel — the reads after a nav are
//! all polling (`wait_for_*`), so they need no inline settle.
//!
//! The SELF Tiers-tab **author management** (§1 My tiers / §2 Pending requests /
//! §3 Subscribers / §4 Payment providers / §5 Claim codes — monetization
//! Pillar 1/3) lives in [`tiers`]; this file is the page shell that owns the
//! header, the edit form, the OTHER offers browse, and the dispatch for both.
//!
//! OTHER's `profile-request-contact-button` is the knock, sent through the
//! contacts page's own `crate::contacts::send_knock` and routed to the viewed
//! profile's home nest (`fauna_client_profile::knock_recipient_nest_url`); a
//! supervised ward's typed refusal reveals the same
//! `contact-request-guardian-button` / `contact-request-pending` pair the
//! contacts page renders (`family-safety.md` § Child-initiated contact
//! requests).
//!
//! The Posts tab — the viewed actor's posts as the feed paints them, over the
//! feed manager's author window — lives in [`posts`]. The consumer-side
//! `subscription-settings` page is a separate page, not part of this one
//! (`monetization.md` § Pillar 1 names it as its own slice).

mod posts;
mod private;
pub mod tiers;

pub use posts::at_last_post;
pub(crate) use posts::{open as open_posts, page_error};

use fauna_ui_ids as ids;
use std::sync::Arc;

use fauna_client::{NestClient, upload_public_post_blob};
use fauna_client_contacts::ContactsClient;
#[cfg(feature = "payments")]
use fauna_client_payments::PaymentsClient;
use fauna_client_profile::{
    ProfileClient, ProfileImageEdit, build_edited_profile_with_images, decode_profile_display,
};
use fauna_client_subscriptions::SubscriptionsClient;
use fauna_conversations::address::TypedAddress;
use fauna_core::data::ProfileLink;
use fauna_core::format;
use fauna_core::identity::{ActorId, ActorKeypair};
use fauna_core::secret::SecretArray32;
use fauna_core::subscription::FOLLOWERS_TIER;
use fauna_i18n::strings::{contacts as c, profile as t, subscriptions as s};
use fauna_nest_http::NestContentApi;
use fauna_protocol::subscriptions::{PendingRequest, SubscribeReply, SubscriberEntry, TierItem};
use tokio::sync::mpsc::UnboundedSender;

use crate::app::{App, DataMessage, UiMessage};
use crate::element::{Element, Field, Gesture};
use crate::pages::Page;

/// Which inner tab is showing. Posts is the default ([`posts`]); Tiers hosts the
/// OTHER-profile offers browse and the SELF author management.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Posts,
    Tiers,
}

/// The SELF text-only edit-form buffers (`profile-edit-*`). Open ⇒ `Some`.
/// `base_body` is the current stored profile bytes — the read-modify-write base
/// `build_edited_profile` preserves the non-display fields from (`None` = first
/// publish).
#[derive(Default)]
struct EditForm {
    base_body: Option<Vec<u8>>,
    /// The owner's last identity-signed row the nest served beside a delegated
    /// `base_body` — the word the save takes `audience` and the harvest
    /// anchors from over an external app's edit (`profile.md` § After an
    /// identity succession → base-admission rule 3). `None` beside a direct
    /// base, or when the nest withheld it.
    last_direct_body: Option<Vec<u8>>,
    display_name: String,
    bio: String,
    /// Repeatable `profile-edit-link-*` rows: `(label, url)`.
    links: Vec<(String, String)>,
    /// `profile-edit-avatar` — a local path staged for upload, not an OS file
    /// picker (`tui.md` § Declared platform absences 4, the `compose-file`
    /// shape). Empty = no new picture picked this edit.
    avatar_path: String,
    /// `profile-edit-avatar-remove-button` was tapped — a typed `avatar_path`
    /// overrides this (picking wins over removing).
    avatar_clear: bool,
    /// `profile-edit-banner` — same staged-path shape as `avatar_path`.
    banner_path: String,
    /// `profile-edit-banner-remove-button` was tapped.
    banner_clear: bool,
    /// Whether the stored profile is open to other nests — the base the
    /// `profile-edit-audience-toggle` starts from (`profile.md` § Where logic
    /// lives → *The remote read*), as the shared own-form projection resolves
    /// it: exactly what an untouched save signs.
    audience_base: bool,
    /// The toggle's staged value, `None` while it still shows the base: a save
    /// that never moved it leaves the stored audience alone (one this build
    /// does not name included).
    audience_edit: Option<bool>,
}

impl EditForm {
    /// What `profile-edit-audience-toggle` shows.
    fn audience_everyone(&self) -> bool {
        self.audience_edit.unwrap_or(self.audience_base)
    }

    /// Flip the toggle; flipping it back to the base un-stages it.
    fn toggle_audience(&mut self) {
        let next = !self.audience_everyone();
        self.audience_edit = (next != self.audience_base).then_some(next);
    }
}

/// Resolve one image field's edit-form buffer into what `Op::Save` must do:
/// a typed path wins over a remove tap (mirrors linux's "picking overrides
/// removing" for the same reason a human's last action should win).
pub(crate) enum StagedImage {
    Keep,
    Clear,
    Upload(String),
}

fn staged_image(path: &str, clear: bool) -> StagedImage {
    let path = path.trim();
    if !path.is_empty() {
        StagedImage::Upload(path.to_string())
    } else if clear {
        StagedImage::Clear
    } else {
        StagedImage::Keep
    }
}

/// The profile page's state, hung off [`App`]. Like contacts/notifications the
/// page holds no observer-backed manager (`profile.md` § Persistence: fetched on
/// demand, minimal local cache); it stores the identity inputs + the last fetch.
#[derive(Default)]
pub struct ProfileState {
    /// The OTHER actor being viewed (hex); `None` = the viewer's own profile.
    /// This single field is `is_self` (`profile.md`: "everything below branches
    /// on `is_self`").
    pub viewing: Option<String>,
    /// The live WS-RPC channel, installed at the post-auth hook. `None` pre-auth.
    nest: Option<Arc<NestClient>>,
    /// The HTTP/bulk plane for the avatar/banner blob upload — the same shared
    /// `fauna_client::upload_public_post_blob` feed's `compose-file` rides
    /// (`media.md` § Encryption at rest: avatar/banner are "the same shape" as
    /// public-post attachments, so this is the identical upload path, not a new
    /// mechanism). `None` pre-auth.
    content: Option<Arc<dyn NestContentApi>>,
    /// The actor's signing secret — for `build_edited_profile` (sign) and
    /// `subscribe_publishing_ek` (the subscriber keypair). A `SecretArray32`
    /// (not a bare `[u8; 32]`) so it zeroizes on drop.
    secret: Option<SecretArray32>,
    /// The actor's period-key custody (`fauna.state.subscriptions`) the Tiers
    /// tab's create / approve / remove mint under. `None` pre-auth.
    period_keys: Option<fauna_client_subscriptions::SharedPeriodKeyStore>,
    /// The viewer's own actor id hex (`is_self` fallback + SELF header fallback).
    self_actor_id: String,
    /// The viewer's own `<handle>@<current primary>` (SELF header fallback below
    /// the published display name) — `session::session_display_handle`, seeded
    /// at attach and re-pushed on every `SelfAddressRefreshed`, never baked
    /// (`mail-primary-domain-rename.md` § Display-handle interaction). `None`
    /// until it resolves; the header then shows the actor id, never a bare handle.
    self_address: Option<String>,
    /// The published display name fetched via `fauna.profile.get`, upgrading the
    /// header over the handle/actor_id fallback. `None` until fetched / on
    /// `not_found`.
    header_name: Option<String>,
    /// The stored pictures the header's profile names, and the nest they rest
    /// on — from the same read as [`Self::header_name`].
    pictures: HeaderPictures,
    /// The header pictures' rasterized art, once fetched ([`fetch_pictures`]).
    art: HeaderArt,
    /// OTHER: whether the follow subscribe landed (label Follow → Following).
    followed: bool,
    /// The actor id `profile-actor-id-copy-btn` last put on the clipboard for
    /// this open, painted back onto the button as its `copied` attr — the value
    /// that reached `copy_to_clipboard`, never re-derived, so a test can assert
    /// what was copied (no e2e driver reads the OS clipboard). Cleared by
    /// [`Self::reset_for_open`].
    copied: Option<String>,
    tab: Tab,
    /// SELF: the edit form when open.
    edit: Option<EditForm>,
    /// OTHER: whether the viewer currently blocks this actor (drives the
    /// Block⇄Unblock toggle label). `None` until the open-time read (or the
    /// viewer's own press) says which edge this open is on — the toggle is
    /// disabled until then, because a press on an unknown edge can only guess,
    /// and guessing not-blocked on an actor already blocked re-blocks.
    is_blocked: Option<bool>,
    /// Which open the in-flight hydration reads belong to — bumped by
    /// [`Self::reset_for_open`], captured by [`spawn_open_refresh`], and checked
    /// in [`apply_outcome`]. A read issued for the actor you were looking at a
    /// moment ago must not land on the one you are looking at now.
    open_epoch: u64,
    /// OTHER: set once the viewer's own Block⇄Unblock press has authored
    /// [`Self::is_blocked`] for the CURRENT open. See [`Outcome::BlockState`] —
    /// the open-time read is issued before any press, so once this is set its
    /// reply describes a superseded edge.
    block_user_authored: bool,
    /// OTHER Tiers tab: the creator's offered tiers.
    offers: Vec<TierItem>,
    /// OTHER Tiers tab: the viewer's confirmed held tier (`status.get`).
    status_tier: Option<String>,
    /// OTHER Tiers tab: a transient post-click `Queued` flag for one tier
    /// (encrypted-mode subscribe), shown until the next `status.get` confirms it.
    pending_tier: Option<String>,
    /// SELF Tiers tab: the author-management sections §§1–5 ([`tiers`]).
    pub(crate) author: tiers::AuthorState,
    /// This box's nest base URL — the §4 webhook-URL preview's prefix
    /// (`fauna_payments::webhook_url`), the knock payload's sender origin, and
    /// the "is it this nest?" side of [`Self::actor_home`]. Empty pre-auth.
    nest_url: String,
    /// OTHER: the viewed actor's home nest when it is another one (`None` =
    /// this nest) — where `profile-request-contact-button`'s knock goes and
    /// where `profile-follow-button`'s follow is carried to
    /// (`federated-follow.md` § Decision 2). The shared
    /// `fauna_client_profile::actor_home` rule: seeded from [`Self::carried_home`]
    /// at the open ([`Self::seed_actor_home`]), replaced by the epoch-checked
    /// open read's answer over the profile it fetched.
    actor_home: Option<String>,
    /// OTHER: the open target's routing hint — the nest the door met the actor
    /// on (a Find User hit, a relayed post's origin, a contact edge). Never
    /// identity; [`Self::actor_home`] is what the follow and knock read.
    carried_home: Option<String>,
    /// OTHER: the knock to this actor landed (the button flips to "Sent").
    knock_sent: bool,
    /// OTHER: the nest refused the knock with the typed guardian-approval error
    /// — reveals `contact-request-guardian-button` (`family-safety.md`
    /// § Child-initiated contact requests). Per open, like `knock_sent`: the
    /// refusal belongs to the actor it was refused for.
    guardian_refused: bool,
    /// OTHER: this session just sent the guardian ask for this actor. The
    /// durable half is `FamilyState::contact_ask_pending`; this flag only makes
    /// the render answer before the re-read lands.
    contact_ask_sent: bool,
    /// OTHER: the private contact overlay projection (`contacts.md` § The
    /// private overlay) — the live source the header's nickname and the
    /// private section's untouched fields read ([`private`]). Set by
    /// [`App::open_profile`]; `None` before a conversations session exists.
    pub(crate) overlays: Option<Arc<fauna_conversations::contacts::ContactsCache>>,
    /// OTHER: the private section's staged edits — the shared staging rule
    /// (`fauna_core::contact_overlay::OverlayEditor`).
    private_edit: fauna_core::contact_overlay::OverlayEditor,
    /// OTHER: `profile-label-field`'s buffer.
    label_input: String,
    /// The Posts tab's author window ([`posts`]) — minted afresh on every open
    /// of the tab, `None` before the first (and pre-auth).
    pub(crate) posts: Option<Arc<crate::feed::CliFeedManager>>,
}

/// A report sheet's *also block this person* landed on `actor`
/// (`crate::report`): if that is the profile open now, its Block⇄Unblock
/// toggle reads the new edge — the same fold [`Outcome::BlockToggled`] makes
/// for the page's own button, so a later open-time read cannot overwrite it.
pub(crate) fn note_blocked_by_report(state: &mut ProfileState, actor: &str) {
    if state.viewing.as_deref() == Some(actor) {
        state.is_blocked = Some(true);
        state.block_user_authored = true;
    }
}

impl ProfileState {
    fn is_self(&self) -> bool {
        self.viewing.is_none()
    }

    /// The nest base URL the §4 webhook preview is built on.
    #[cfg(feature = "payments")]
    pub(crate) fn nest_url(&self) -> &str {
        &self.nest_url
    }

    /// Put the page on a given inner tab without going through the gesture —
    /// for tests that need the Tiers tab painted but not its network load
    /// (`automation.rs`'s registry-shape tests).
    #[cfg(test)]
    pub(crate) fn set_tab_for_test(&mut self, tab: Tab) {
        self.tab = tab;
    }

    /// The viewer's own actor id hex — the §4 webhook URL's author path segment
    /// (always the viewer, never [`Self::actor_id`]'s possibly-OTHER target).
    #[cfg(feature = "payments")]
    pub(crate) fn self_actor_id_hex(&self) -> &str {
        &self.self_actor_id
    }

    /// The Posts tab window's rows — what the feed's image fetch also walks
    /// (`crate::feed::kick_image_fetches`), since the tab paints from the same
    /// cache.
    pub(crate) fn posts_snapshot(&self) -> Option<fauna_feed::FeedSnapshot> {
        self.posts.as_ref().map(|w| w.snapshot())
    }

    /// The actor whose profile is shown — the OTHER hex, or the viewer's own.
    fn actor_id(&self) -> &str {
        self.viewing.as_deref().unwrap_or(&self.self_actor_id)
    }

    /// Reset the per-view transient state on (re)open — a fresh target must not
    /// inherit the prior actor's header/offers/block state (linux rebuilds the
    /// whole view per `open_profile`). Called by [`App::open_profile`].
    pub(crate) fn reset_for_open(&mut self) {
        self.tab = Tab::Posts;
        self.edit = None;
        self.header_name = None;
        self.pictures = HeaderPictures::default();
        self.art = HeaderArt::default();
        self.followed = false;
        self.copied = None;
        self.is_blocked = None;
        // Retire whatever the previous open still has in flight, and hand the
        // fresh reads the authority the press guard below would otherwise deny
        // them for the rest of the session.
        self.open_epoch = self.open_epoch.wrapping_add(1);
        self.block_user_authored = false;
        self.offers.clear();
        self.status_tier = None;
        self.pending_tier = None;
        self.author.reset();
        self.actor_home = None;
        self.carried_home = None;
        self.knock_sent = false;
        self.guardian_refused = false;
        self.contact_ask_sent = false;
        // Staged private edits belong to the person they were typed for.
        self.private_edit.reset();
        self.label_input.clear();
        // The previous actor's posts; the open mints this actor's window.
        self.posts = None;
    }

    /// Seed the actor-home slot from the open target's carried `home`, through
    /// the shared rule with no profile yet (`profile.md` § Where logic lives →
    /// *The actor's home*) — so a follow pressed before the header read answers
    /// already routes. Called after [`Self::reset_for_open`]; SELF has no
    /// other actor's home, so a hint there is dropped.
    pub fn seed_actor_home(&mut self, home: Option<String>) {
        self.carried_home = self.viewing.as_ref().and(home);
        self.actor_home = self.viewing.as_deref().and_then(|hex| {
            fauna_client_profile::actor_home(
                self.carried_home.as_deref(),
                &ActorId::from_hex(hex).ok()?,
                None,
                &self.nest_url,
            )
        });
    }
}

/// Build the page state at the post-auth hook (`session::establish`). No eager
/// fetch — the page hydrates when it is opened (`App::open_profile` →
/// [`spawn_open_refresh`]), the linux on-visible-build shape.
pub fn init(
    nest: Arc<NestClient>,
    content: Arc<dyn NestContentApi>,
    secret: SecretArray32,
    period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
    self_actor_id: String,
    self_address: Option<String>,
    nest_url: String,
) -> ProfileState {
    ProfileState {
        nest: Some(nest),
        content: Some(content),
        secret: Some(secret),
        period_keys: Some(period_keys),
        self_actor_id,
        self_address,
        nest_url,
        ..ProfileState::default()
    }
}

/// Land a newly resolved display handle (the `SelfAddressRefreshed` push). A
/// `None` keeps what the header already shows — never push a partial.
pub fn set_self_address(st: &mut ProfileState, address: Option<String>) {
    if address.is_some() {
        st.self_address = address;
    }
}

/// Fire-and-forget the reads a profile open implies — the header display name
/// (SELF or OTHER) and, for OTHER, the initial block-toggle state. The results
/// land through the channel; the reads after a nav are all polling, so they need
/// no inline settle. Called by [`App::open_profile`].
///
/// Both replies carry the [`ProfileState::open_epoch`] they were issued under.
/// Fire-and-forget is only safe if the reply knows which question it answered:
/// these are reads of a *particular actor at a particular open*, and nothing
/// stops the viewer navigating on, or pressing the toggle, while they are in
/// flight. [`apply_outcome`] is where a superseded answer gets dropped.
///
/// `blobs` is the session's post-blob host pick (`crate::feed::blob_hosts`),
/// which the header's pictures are fetched through once the name has landed.
pub fn spawn_open_refresh(
    state: &ProfileState,
    tx: &UnboundedSender<UiMessage>,
    conversations: Option<std::sync::Weak<fauna_conversations::ConversationsSession>>,
    session_generation: u64,
    blobs: Option<fauna_client::PostBlobHosts>,
) {
    let Some(nest) = state.nest.clone() else {
        return;
    };
    let actor_id = state.actor_id().to_string();
    let is_other = state.viewing.is_some();
    let epoch = state.open_epoch;
    let own_nest_url = state.nest_url.clone();
    let carried = state.carried_home.clone();
    // Where the header read falls through to on a local `not_found`: the
    // actor-home slot as the open seeded it (`None` for SELF and this nest).
    let relay_home = state.actor_home.clone().filter(|_| is_other);
    let tx = tx.clone();
    tokio::spawn(async move {
        let HeaderFetch {
            name,
            bytes,
            pictures,
        } = fetch_header_profile(Arc::clone(&nest), &actor_id, relay_home.as_deref()).await;
        // The actor's home rides the same read: the profile the header renders
        // beats the home the open target carried (the shared rule).
        let actor_home = match ActorId::from_hex(&actor_id) {
            Ok(actor) if is_other => fauna_client_profile::actor_home(
                carried.as_deref(),
                &actor,
                bytes.as_deref(),
                &own_nest_url,
            ),
            _ => None,
        };
        let _ = tx.send(UiMessage::Data(DataMessage::Page(
            session_generation,
            crate::app::PageOutcome::Profile(Outcome::HeaderName {
                epoch,
                name,
                actor_home,
                pictures: pictures.clone(),
            }),
        )));
        // The pictures paint after the name: a slow or failing home never
        // holds the header's text back.
        if let Some(blobs) = blobs.filter(|_| !pictures.is_empty()) {
            let art = fetch_pictures(&blobs, &pictures).await;
            let _ = tx.send(UiMessage::Data(DataMessage::Page(
                session_generation,
                crate::app::PageOutcome::Profile(Outcome::Pictures {
                    epoch,
                    art: Box::new(art),
                }),
            )));
        }
        if is_other {
            let blocked = fetch_block_state(&nest, &actor_id).await;
            let _ = tx.send(UiMessage::Data(DataMessage::Page(
                session_generation,
                crate::app::PageOutcome::Profile(Outcome::BlockState { epoch, blocked }),
            )));
            // The page-path harvest (OTHER only: harvesting one's own profile
            // would seed anchor slots about oneself), through the account's
            // anchor store — lent to the session's manager at the
            // account-store-ready edge; before it, nothing is seeded and the
            // roster sweep still covers the peer.
            let store = conversations
                .as_ref()
                .and_then(std::sync::Weak::upgrade)
                .and_then(|session| session.manager().peer_anchor_store());
            if let (Some(store), Some(bytes), Ok(peer)) = (
                store,
                bytes,
                fauna_core::identity::ActorId::from_hex(&actor_id),
            ) {
                harvest_page_read(&peer, &bytes, store.as_ref(), conversations).await;
            }
        }
    });
}

// ── Field access (the SELF edit form + the Tiers-tab forms) ─────────────────

/// A profile-page editable field (`crate::profile`) — the SELF edit form, plus
/// the Tiers-tab §1/§4 form buffers behind [`ProfileField::Tier`].
///
/// Every non-`Tier` variant is an edit-form buffer (the shared `Edit` prefix is
/// the mandated cross-page naming, not a lint-worthy accident).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProfileField {
    /// A SELF Tiers-tab form buffer (§1 tier form / §4 provider form) — see
    /// [`tiers::TierField`]. Kept as one arm so the page has a single
    /// `Field::Profile` dispatch.
    Tier(tiers::TierField),
    /// `profile-edit-display-name` — the SELF edit form's display-name buffer.
    /// A local buffer committed only on Save (`build_edited_profile`).
    DisplayName,
    /// `profile-edit-bio` — the bio buffer (same commit-on-Save shape).
    Bio,
    /// `profile-edit-link-label[i]` — link row `i`'s label buffer. Indexed
    /// because the link list is repeatable data, like the provider-credential
    /// fields carry their own id.
    LinkLabel(usize),
    /// `profile-edit-link-url[i]` — link row `i`'s url buffer (indexed).
    LinkUrl(usize),
    /// `profile-edit-avatar` — the staged local avatar path (a typed path, not
    /// an OS file picker; `compose-file`'s shape).
    Avatar,
    /// `profile-edit-banner` — the staged local banner path.
    Banner,
    /// `profile-nickname-field` — the private section's nickname (OTHER;
    /// [`private`]).
    PrivateNickname,
    /// `profile-notes-field` — the private section's notes.
    PrivateNotes,
    /// `profile-label-field` — the label about to be added.
    PrivateLabel,
}

pub fn field(state: &ProfileState, field: &ProfileField) -> String {
    // The Tiers-tab forms are independent of the edit form, so they resolve
    // before the `edit` guard below.
    if let ProfileField::Tier(f) = field {
        return tiers::field(state, f);
    }
    if let Some(value) = private::field(state, field) {
        return value;
    }
    let Some(edit) = &state.edit else {
        return String::new();
    };
    match field {
        ProfileField::Tier(_)
        | ProfileField::PrivateNickname
        | ProfileField::PrivateNotes
        | ProfileField::PrivateLabel => unreachable!("handled above"),
        ProfileField::DisplayName => edit.display_name.clone(),
        ProfileField::Bio => edit.bio.clone(),
        ProfileField::LinkLabel(i) => edit
            .links
            .get(*i)
            .map(|(l, _)| l.clone())
            .unwrap_or_default(),
        ProfileField::LinkUrl(i) => edit
            .links
            .get(*i)
            .map(|(_, u)| u.clone())
            .unwrap_or_default(),
        ProfileField::Avatar => edit.avatar_path.clone(),
        ProfileField::Banner => edit.banner_path.clone(),
    }
}

/// All edit-form fields are local buffers committed only on Save (the
/// read-modify-write `build_edited_profile`), so a write lands synchronously and
/// returns no pending work.
pub fn set_field(state: &mut ProfileState, field: ProfileField, value: String) {
    if let ProfileField::Tier(f) = field {
        tiers::set_field(state, f, value);
        return;
    }
    if private::set_field(state, &field, value.clone()) {
        return;
    }
    let Some(edit) = &mut state.edit else {
        return;
    };
    match field {
        ProfileField::Tier(_)
        | ProfileField::PrivateNickname
        | ProfileField::PrivateNotes
        | ProfileField::PrivateLabel => unreachable!("handled above"),
        ProfileField::DisplayName => edit.display_name = value,
        ProfileField::Bio => edit.bio = value,
        ProfileField::LinkLabel(i) => {
            if let Some(row) = edit.links.get_mut(i) {
                row.0 = value;
            }
        }
        ProfileField::LinkUrl(i) => {
            if let Some(row) = edit.links.get_mut(i) {
                row.1 = value;
            }
        }
        // Staging only, like `FeedField::ComposeFile` — the upload happens at
        // Save (`staged_image` reads whichever of path/clear won).
        ProfileField::Avatar => edit.avatar_path = value,
        ProfileField::Banner => edit.banner_path = value,
    }
}

// ── Gestures ─────────────────────────────────────────────────────────────────

/// A gesture on the profile page. Each maps onto a shared-Rust call or a local
/// state change (`profile.md` § User actions).
#[derive(Debug, Clone)]
pub enum Action {
    /// `profile-actor-id-copy-btn` — copy the viewed actor id (OSC 52).
    Copy,
    /// `profile-edit-button` (SELF) — open the edit form (fetch the RMW base).
    OpenEdit,
    /// `profile-edit-save-button` — build + sign + `fauna.profile.set`, refetch
    /// the header.
    SaveEdit,
    /// `profile-edit-cancel-button` — close the form.
    CancelEdit,
    /// `profile-edit-link-add-button` — append an empty link row.
    AddLink,
    /// `profile-edit-link-remove-button[i]` — drop link row `i`.
    RemoveLink(usize),
    /// `profile-edit-avatar-remove-button` — stage clearing the avatar on save
    /// (a typed `profile-edit-avatar` path taps back over this).
    ClearAvatar,
    /// `profile-edit-banner-remove-button` — stage clearing the banner on save.
    ClearBanner,
    /// `profile-edit-audience-toggle` — stage opening (or closing) the profile
    /// to other nests; signed on save.
    ToggleAudience,
    /// `profile-follow-button` (OTHER) — subscribe to the free "followers" tier.
    Follow,
    /// `profile-start-dm-button` (OTHER) — seed the Conversations composer + nav.
    StartDm,
    /// `profile-block-button` (OTHER) — the Block⇄Unblock toggle.
    ToggleBlock,
    /// `profile-request-contact-button` (OTHER) — send the knock, routed by
    /// [`ProfileState::actor_home`].
    RequestContact,
    /// `contact-request-guardian-button` (OTHER, supervised ward) — ask the
    /// guardian to approve this actor, offered only after the knock came back
    /// with the typed guardian refusal.
    AskGuardian,
    /// `profile-posts-tab` — the default; every open mints a fresh author
    /// window and reads its first page ([`posts`]).
    ShowPosts,
    /// The Posts tab's next page — the feed's load-more gesture (the ring
    /// arriving on the last card, [`at_last_post`]).
    LoadMorePosts,
    /// A gesture on one of the Posts tab's cards — the feed card's own gesture,
    /// re-routed to the manager its row came from ([`posts`]).
    Post(crate::feed::Action),
    /// `profile-tiers-tab` — OTHER loads the offers browse.
    ShowTiers,
    /// `subscription-offer-subscribe-button[i]` — subscribe to that tier.
    Subscribe(String),
    /// `subscription-offer-payment-link[i]` — open the offer's external
    /// `payment_url` in the OS browser (off-platform checkout). The money
    /// plane's buyer half, with the offer row's link that paints it.
    #[cfg(feature = "payments")]
    OpenPaymentLink(String),

    // ── OTHER private section ([`private`]) ─────────────────────────────
    /// `profile-label-add-button` — stage the typed label.
    AddPrivateLabel,
    /// `profile-label-remove-button[i]` — stage removing label row `i`.
    RemovePrivateLabel(usize),
    /// `profile-label-suggestion[i]` — stage the `i`th offered label.
    PickLabelSuggestion(usize),
    /// `profile-private-save-button` — write the changed registers.
    SavePrivate,

    // ── SELF Tiers tab §§1–5 (`tiers`) ──────────────────────────────────
    /// `subscription-tier-create-button` (`None`) /
    /// `subscription-tier-edit-button[i]` (`Some(name)`) — open the §1 form.
    OpenTierForm(Option<String>),
    /// `subscription-tier-form-cancel`.
    CancelTierForm,
    /// `subscription-tier-form-auto-approve` — flip the form's toggle.
    ToggleAutoApprove,
    /// `subscription-tier-form-save` — create or update, then re-read §§1–5.
    SaveTierForm,
    /// `subscription-tier-delete-button[i]` — `tiers.delete`, then re-read.
    DeleteTier(String),
    /// `subscription-request-approve-button[i]` — the transparent mint+upload
    /// (`SubscriptionsAuthor::approve_subscriber`). Carries the ROW INDEX, not
    /// the request id: approve needs the whole [`PendingRequest`] (its tier and
    /// the requester's published ek), which only the row holds.
    ApproveRequest(usize),
    /// `subscription-request-reject-button[i]` — `requests.reject(request_id)`.
    RejectRequest(i64),
    /// `subscription-subscribers-tier-select` — re-read §3 for that tier.
    SelectRosterTier(String),
    /// `subscription-subscriber-remove-button[i]` — the roster-rotating removal
    /// (`SubscriptionsAuthor::remove_subscriber`). Row index for the same reason
    /// as [`Self::ApproveRequest`] (removal needs the subscriber's ActorId).
    RemoveSubscriber(usize),
    // ── §§4–5, the money plane — every variant `payments`-gated ──────────
    // Gated as GESTURES, not merely as senders: criterion 5 of
    // `dynamic-features.md` § What "completely compiled away" means is "no
    // re-enable path", and an action a store-safe build can still dispatch is
    // one. Their element ids go with them (criterion 1).
    /// `subscription-provider-add-button` — open the §4 form.
    #[cfg(feature = "payments")]
    OpenProviderForm,
    /// `subscription-provider-form-cancel`.
    #[cfg(feature = "payments")]
    CancelProviderForm,
    /// `subscription-provider-form-kind` — pick the provider kind (this is what
    /// makes the webhook-URL preview recompute).
    #[cfg(feature = "payments")]
    SetProviderFormKind(String),
    /// `subscription-provider-form-tier-map` — pick the entitled tier.
    #[cfg(feature = "payments")]
    SetProviderFormTier(String),
    /// `subscription-provider-form-webhook-url-copy-button` — OSC 52 copy.
    #[cfg(feature = "payments")]
    CopyWebhookUrl,
    /// `subscription-provider-form-return-url-copy-button` — OSC 52 copy.
    #[cfg(feature = "payments")]
    CopyReturnUrl,
    /// `subscription-provider-form-save` — `providers.set`, then re-read.
    #[cfg(feature = "payments")]
    SaveProviderForm,
    /// `subscription-provider-remove-button[i]` — `providers.remove(kind)`.
    #[cfg(feature = "payments")]
    RemoveProvider(String),
    /// `subscription-claim-tier-select` — which tier a minted code entitles.
    #[cfg(feature = "payments")]
    SelectClaimTier(String),
    /// `subscription-claim-mint-button` — `claims.mint`, then re-read.
    #[cfg(feature = "payments")]
    MintClaim,
    /// `subscription-claim-void-button[i]` — the first press on a row arms the
    /// confirm in place (the same button's text names the consequence); the
    /// second press on that row runs `claims.void(code)`, then re-reads.
    /// Keyed by the CODE, so a re-read that reorders the rows cannot aim an
    /// armed confirm at another code.
    #[cfg(feature = "payments")]
    VoidClaim(String),
    /// The armed §5 confirm's Keep — disarm, nothing sent.
    #[cfg(feature = "payments")]
    CancelVoidClaim,
}

/// Apply a gesture's local half and hand back its network half, if any — the
/// contacts split (the agent awaits, the keyboard spawns).
impl Action {
    /// The wire kind this gesture issues — the offline gate's input
    /// (`crate::element::Gesture::wire_kind`). Exhaustive with no fallback
    /// arm, so a new profile gesture must answer the offline question.
    ///
    /// The Tiers tab is where this page splits: **authoring** a tier is
    /// `OfflineSafe` (the author's own catalog), while **money** — following,
    /// subscribing, payment providers, claim codes — is `OnlineOnly`, because
    /// a nest has to arbitrate it. Those are the affordances that desensitize.
    pub fn wire_kind(&self) -> Option<&'static str> {
        match self {
            Action::SaveEdit => Some("fauna.profile.set"),
            // Following IS a subscription to the author's `followers` tier
            // (`Op::Follow` — `subscribe(author, "followers")`).
            Action::Follow | Action::Subscribe(_) => Some("fauna.subscriptions.subscribe"),
            // The knock rides `fauna.inbox.send`, the ask its family twin —
            // the same kinds the contacts page's pair sends.
            Action::RequestContact => Some("fauna.inbox.send"),
            Action::AskGuardian => Some("fauna.family.contact.request"),
            Action::DeleteTier(_) => Some("fauna.subscriptions.tiers.delete"),
            Action::ApproveRequest(_) => Some("fauna.subscriptions.requests.approve"),
            Action::RejectRequest(_) => Some("fauna.subscriptions.requests.reject"),
            Action::RemoveSubscriber(_) => Some("fauna.subscriptions.subscribers.remove"),
            #[cfg(feature = "payments")]
            Action::SaveProviderForm => Some("fauna.payments.providers.set"),
            #[cfg(feature = "payments")]
            Action::RemoveProvider(_) => Some("fauna.payments.providers.remove"),
            #[cfg(feature = "payments")]
            Action::MintClaim => Some("fauna.payments.claims.mint"),
            // The arming press is local, but the verb as a whole is the void:
            // offline, the button desensitizes rather than arming a confirm
            // whose second press could only fail.
            #[cfg(feature = "payments")]
            Action::VoidClaim(_) => Some("fauna.payments.claims.void"),

            // State-dependent, so the gate declines rather than guesses (the
            // `crate::moderation::Action::Correct` rule): `ToggleBlock` issues
            // `knocks_block` or `knocks_unblock` depending on the current edge,
            // and `SaveTierForm` issues `tiers.create` or `tiers.update`
            // depending on `form.editing` — only [`apply_local`]'s successor op
            // knows which.
            Action::ToggleBlock | Action::SaveTierForm => None,

            // The Posts tab's reads, and its cards' own gestures answering as
            // they do on the feed.
            Action::LoadMorePosts => Some("fauna.feed.author.posts"),
            Action::Post(action) => action.wire_kind(),

            // Local. Clipboard writes, tab switches whose refetch is a `Read`,
            // the edit/tier/provider form buffers, the Start-DM nav glue
            // (`start_dm` — "pure nav glue, no new kind"), and the OS handoff
            // of a payment URL (its own arm, so the flavor without the
            // variant compiles the list unchanged).
            #[cfg(feature = "payments")]
            Action::OpenPaymentLink(_) => None,
            Action::Copy
            | Action::OpenEdit
            | Action::CancelEdit
            | Action::AddLink
            | Action::RemoveLink(_)
            | Action::ClearAvatar
            | Action::ClearBanner
            | Action::ToggleAudience
            | Action::StartDm
            | Action::ShowPosts
            | Action::ShowTiers
            | Action::OpenTierForm(_)
            | Action::CancelTierForm
            | Action::ToggleAutoApprove
            | Action::SelectRosterTier(_) => None,
            // The private section writes the viewer's own account plane
            // (a local seal, published by the pump) — no nest arbitrates it,
            // so it is offline-safe like every other own-plane edit.
            Action::AddPrivateLabel
            | Action::RemovePrivateLabel(_)
            | Action::PickLabelSuggestion(_)
            | Action::SavePrivate => None,
            // The §§4–5 local half, spelled as its own arm only because
            // `#[cfg]` cannot sit on one alternative of an or-pattern.
            #[cfg(feature = "payments")]
            Action::CopyWebhookUrl
            | Action::CopyReturnUrl
            | Action::OpenProviderForm
            | Action::CancelProviderForm
            | Action::SetProviderFormKind(_)
            | Action::SetProviderFormTier(_)
            | Action::SelectClaimTier(_)
            | Action::CancelVoidClaim => None,
        }
    }
}

pub fn apply_local(app: &mut App, action: Action) -> Option<Op> {
    // Start-DM needs `app.conversations` + `app.page`, so it can't run under the
    // `&mut app.profile` borrow below — handle it first (pure nav glue, no op).
    if let Action::StartDm = action {
        start_dm(app);
        return None;
    }
    // The private section reads the conversations manager and the account
    // store off `app`, so it too runs outside the page-state borrow.
    if matches!(
        action,
        Action::AddPrivateLabel
            | Action::RemovePrivateLabel(_)
            | Action::PickLabelSuggestion(_)
            | Action::SavePrivate
    ) {
        return private::apply_local(app, action);
    }
    // The Posts tab mints its window off the timeline's manager and routes its
    // cards' gestures to the feed — both reach past the page state ([`posts`]).
    match action {
        Action::ShowPosts => {
            app.profile.tab = Tab::Posts;
            return posts::open(app);
        }
        Action::LoadMorePosts => return posts::load_more(app),
        Action::Post(action) => return posts::route(app, action),
        _ => {}
    }
    // Whom this identity succeeded from, per the account registry — the only
    // evidence that admits a stored base signed by someone else. Read before
    // the page-state borrow below, and only for the one action that signs.
    let predecessors = match action {
        Action::SaveEdit => crate::session::profile_predecessors(app, &app.profile.self_actor_id),
        _ => Vec::new(),
    };
    // Where the edit form's base load records a link it proves — taken here
    // for the same borrow reason.
    let accounts = matches!(action, Action::OpenEdit).then(|| crate::session::registry(app));
    // The refetched header's pictures after a save — the session's host pick,
    // off the feed's state for the same borrow reason.
    let blobs = matches!(action, Action::SaveEdit)
        .then(|| crate::feed::blob_hosts(&app.feed))
        .flatten();
    // The §4 return-URL display builds on the creator's web-site origin, which
    // lives in the settings page's hydrated state — read before the page-state
    // borrow below. `None` also covers "not hydrated yet".
    #[cfg(feature = "payments")]
    let site_origin = crate::settings::web::site_link(&app.settings).origin;
    #[cfg(feature = "payments")]
    let web_origin_handle = app
        .settings
        .web
        .view
        .is_none()
        .then(|| app.settings.web_origin_handle());
    let st = &mut app.profile;
    match action {
        Action::StartDm
        | Action::AddPrivateLabel
        | Action::RemovePrivateLabel(_)
        | Action::PickLabelSuggestion(_)
        | Action::SavePrivate => unreachable!("handled above"),
        Action::Copy => {
            let id = st.actor_id().to_string();
            crate::wizard::copy_to_clipboard(&id);
            st.copied = Some(id);
            None
        }
        Action::OpenEdit => {
            // Show the form immediately; the async fetch seeds the RMW base.
            st.edit = Some(EditForm::default());
            Some(Op::LoadEditBase {
                nest: st.nest.clone()?,
                secret: st.secret.clone()?,
                accounts: accounts?,
            })
        }
        Action::CancelEdit => {
            st.edit = None;
            app.errors.remove(&Page::Profile);
            None
        }
        Action::AddLink => {
            if let Some(edit) = &mut st.edit {
                edit.links.push((String::new(), String::new()));
            }
            None
        }
        Action::RemoveLink(i) => {
            if let Some(edit) = &mut st.edit
                && i < edit.links.len()
            {
                edit.links.remove(i);
            }
            None
        }
        Action::ClearAvatar => {
            if let Some(edit) = &mut st.edit {
                edit.avatar_clear = true;
                edit.avatar_path.clear();
            }
            None
        }
        Action::ClearBanner => {
            if let Some(edit) = &mut st.edit {
                edit.banner_clear = true;
                edit.banner_path.clear();
            }
            None
        }
        Action::ToggleAudience => {
            if let Some(edit) = &mut st.edit {
                edit.toggle_audience();
            }
            None
        }
        Action::SaveEdit => {
            let edit = st.edit.as_ref()?;
            Some(Op::Save {
                nest: st.nest.clone()?,
                content: st.content.clone(),
                blobs,
                secret: st.secret.clone()?,
                self_actor_id: st.self_actor_id.clone(),
                base_body: edit.base_body.clone(),
                last_direct_body: edit.last_direct_body.clone(),
                predecessors,
                display_name: non_empty(&edit.display_name),
                bio: non_empty(&edit.bio),
                links: edit
                    .links
                    .iter()
                    .filter(|(l, u)| !l.trim().is_empty() || !u.trim().is_empty())
                    .map(|(l, u)| ProfileLink {
                        label: l.clone(),
                        uri: u.clone(),
                    })
                    .collect(),
                avatar: staged_image(&edit.avatar_path, edit.avatar_clear),
                banner: staged_image(&edit.banner_path, edit.banner_clear),
                audience: edit.audience_edit,
            })
        }
        Action::Follow => Some(Op::Follow {
            nest: st.nest.clone()?,
            secret: st.secret.clone()?,
            author: st.viewing.clone()?,
            home_nest_url: st.actor_home.clone(),
        }),
        Action::ToggleBlock => Some(Op::ToggleBlock {
            nest: st.nest.clone()?,
            actor_id: st.viewing.clone()?,
            currently_blocked: st.is_blocked?,
        }),
        Action::RequestContact => {
            if st.knock_sent {
                return None; // the button is disabled; belt-and-suspenders
            }
            Some(Op::SendKnock {
                nest: st.nest.clone()?,
                node_url: st.nest_url.clone(),
                secret: st.secret.as_ref()?.to_array(),
                recipient: st.viewing.clone()?,
                recipient_nest_url: st.actor_home.clone(),
            })
        }
        Action::AskGuardian => {
            // A re-ask while one is pending is a quiet nest-side no-op anyway,
            // but not issuing it keeps the affordance honest about its state.
            if st.contact_ask_sent {
                return None;
            }
            Some(Op::AskGuardian {
                nest: st.nest.clone()?,
                peer: st.viewing.clone()?,
            })
        }
        // Handled above, outside the page-state borrow.
        Action::ShowPosts | Action::LoadMorePosts | Action::Post(_) => None,
        Action::ShowTiers => {
            st.tab = Tab::Tiers;
            let nest = st.nest.clone()?;
            // OTHER loads the offers browse; SELF loads the §§1–5 author
            // management (`tiers`).
            match st.viewing.clone() {
                Some(author) => Some(Op::LoadOffers { nest, author }),
                None => Some(Op::LoadAuthor {
                    nest,
                    roster_tier: st.author.roster_tier.clone(),
                }),
            }
        }
        Action::Subscribe(tier) => {
            // Optimistic transient: show "Pending approval" until the refetch
            // settles it (Active on an auto-approve/plaintext nest).
            st.pending_tier = Some(tier.clone());
            Some(Op::Subscribe {
                nest: st.nest.clone()?,
                secret: st.secret.clone()?,
                author: st.viewing.clone()?,
                tier,
            })
        }
        // Refused for a non-`https` scheme via the shared `fauna_core::
        // subscription::is_safe_payment_url` guard (F-CL2 anti-phishing-redirect
        // class) — author-supplied content, same check the feed's
        // `gated-post-payment-link` applies.
        #[cfg(feature = "payments")]
        Action::OpenPaymentLink(url) => {
            if fauna_core::subscription::is_safe_payment_url(&url) {
                crate::os_open::open(&url);
            } else {
                app.errors
                    .insert(Page::Profile, s::UNSAFE_PAYMENT_URL.to_string());
            }
            None
        }

        // ── SELF Tiers tab §§1–5 ────────────────────────────────────────
        Action::OpenTierForm(name) => {
            let existing = name
                .as_ref()
                .and_then(|n| st.author.tiers.iter().find(|t| &t.name == n));
            st.author.form = Some(match existing {
                // Edit: seed every buffer from the row so a Save that touches
                // one field cannot blank the rest (the RMW shape the edit form
                // uses for the profile body).
                Some(t) => tiers::TierForm {
                    editing: Some(t.name.clone()),
                    name: t.name.clone(),
                    rank: t.rank.to_string(),
                    description: t.description.clone().unwrap_or_default(),
                    price_hint: t.price_hint.clone().unwrap_or_default(),
                    // The reverse of `TierAskingPrice::from_sats` — pre-fill
                    // the edit form with the tier's current price in sats, or
                    // empty for an unpriced tier / a unit this build cannot
                    // interpret (fail-closed: an edit that saves without
                    // touching this field must keep the current price, never
                    // silently clear it — see `asking_price`'s save-side
                    // "None means keep-current" handling below).
                    asking_price: t
                        .asking_price
                        .as_ref()
                        .and_then(|p| p.to_sats())
                        .map(|s| s.to_string())
                        .unwrap_or_default(),
                    payment_url: t.payment_url.clone().unwrap_or_default(),
                    auto_approve: t.auto_approve,
                },
                None => tiers::TierForm::default(),
            });
            None
        }
        Action::CancelTierForm => {
            st.author.form = None;
            app.errors.remove(&Page::Profile);
            None
        }
        Action::ToggleAutoApprove => {
            if let Some(f) = st.author.form.as_mut() {
                f.auto_approve = !f.auto_approve;
            }
            None
        }
        Action::SaveTierForm => {
            let form = st.author.form.clone()?;
            Some(Op::SaveTier {
                nest: st.nest.clone()?,
                secret: st.secret.clone()?,
                period_keys: st.period_keys.clone()?,
                roster_tier: st.author.roster_tier.clone(),
                form,
            })
        }
        Action::DeleteTier(name) => Some(Op::DeleteTier {
            nest: st.nest.clone()?,
            roster_tier: st.author.roster_tier.clone(),
            name,
        }),
        Action::ApproveRequest(i) => {
            let request = st.author.requests.get(i)?.clone();
            let op = Op::ApproveRequest {
                nest: st.nest.clone()?,
                secret: st.secret.clone()?,
                period_keys: st.period_keys.clone()?,
                roster_tier: st.author.roster_tier.clone(),
                request: Box::new(request),
            };
            // The mint+upload can take a while — paint
            // `subscription-request-busy` for its whole duration. Raised only
            // once the op is certain: an early raise on a path that then returns
            // `None` would leave the marker painted with nothing in flight to
            // clear it.
            st.author.busy = true;
            Some(op)
        }
        Action::RejectRequest(request_id) => Some(Op::RejectRequest {
            nest: st.nest.clone()?,
            roster_tier: st.author.roster_tier.clone(),
            request_id,
        }),
        Action::SelectRosterTier(tier) => {
            st.author.roster_tier = tier.clone();
            Some(Op::LoadRoster {
                nest: st.nest.clone()?,
                tier,
            })
        }
        Action::RemoveSubscriber(i) => {
            let subscriber = st.author.subscribers.get(i)?.subscriber_id;
            Some(Op::RemoveSubscriber {
                nest: st.nest.clone()?,
                secret: st.secret.clone()?,
                period_keys: st.period_keys.clone()?,
                tier: st.author.roster_tier.clone(),
                subscriber,
            })
        }
        #[cfg(feature = "payments")]
        Action::OpenProviderForm => {
            st.author.provider_form = Some(tiers::ProviderForm::default());
            // Hydrate the web origin the return-URL display needs, once: the
            // settings page owns refresh after that.
            Some(Op::HydrateWebOrigin {
                nest: st.nest.clone()?,
                handle: web_origin_handle?,
            })
        }
        #[cfg(feature = "payments")]
        Action::CancelProviderForm => {
            st.author.provider_form = None;
            app.errors.remove(&Page::Profile);
            None
        }
        #[cfg(feature = "payments")]
        Action::SetProviderFormKind(kind) => {
            if let Some(f) = st.author.provider_form.as_mut() {
                f.kind = kind;
            }
            None
        }
        #[cfg(feature = "payments")]
        Action::SetProviderFormTier(tier) => {
            if let Some(f) = st.author.provider_form.as_mut() {
                f.tier = tier;
            }
            None
        }
        #[cfg(feature = "payments")]
        Action::CopyWebhookUrl => {
            crate::wizard::copy_to_clipboard(&tiers::webhook_url(st));
            None
        }
        #[cfg(feature = "payments")]
        Action::CopyReturnUrl => {
            if let Some(url) = tiers::return_url(st, site_origin.as_deref()) {
                crate::wizard::copy_to_clipboard(&url);
            }
            None
        }
        #[cfg(feature = "payments")]
        Action::SaveProviderForm => {
            let form = st.author.provider_form.clone()?;
            Some(Op::SaveProvider {
                nest: st.nest.clone()?,
                roster_tier: st.author.roster_tier.clone(),
                form,
            })
        }
        #[cfg(feature = "payments")]
        Action::RemoveProvider(kind) => Some(Op::RemoveProvider {
            nest: st.nest.clone()?,
            roster_tier: st.author.roster_tier.clone(),
            kind,
        }),
        #[cfg(feature = "payments")]
        Action::SelectClaimTier(tier) => {
            st.author.claim_tier = tier;
            None
        }
        #[cfg(feature = "payments")]
        Action::MintClaim => Some(Op::MintClaim {
            nest: st.nest.clone()?,
            roster_tier: st.author.roster_tier.clone(),
            tier: st.author.claim_tier.clone(),
        }),
        #[cfg(feature = "payments")]
        Action::VoidClaim(code) => {
            if st.author.claim_void_armed.as_deref() != Some(code.as_str()) {
                st.author.claim_void_armed = Some(code);
                return None;
            }
            st.author.claim_void_armed = None;
            Some(Op::VoidClaim {
                nest: st.nest.clone()?,
                roster_tier: st.author.roster_tier.clone(),
                code,
            })
        }
        #[cfg(feature = "payments")]
        Action::CancelVoidClaim => {
            st.author.claim_void_armed = None;
            None
        }
    }
}

/// Start-DM: seed the Conversations new-thread composer with the viewed actor
/// and switch to the Conversations page (pure nav glue, no new kind —
/// `profile.md` § Where logic lives → Start DM). The chip display is the actor
/// id hex (the OTHER header holds no cached handle), which the e2e asserts.
fn start_dm(app: &mut App) {
    let Some(hex) = app.profile.viewing.clone() else {
        return;
    };
    let Ok(actor_id) = ActorId::from_hex(&hex) else {
        return;
    };
    if let Some(manager) = app.conversations.manager.clone() {
        manager.start_new_conversation();
        manager.accept_new_thread_chip(TypedAddress::Fauna {
            handle: hex,
            actor_id,
        });
    }
    app.conversations.mode = crate::conversations::Mode::Compose;
    app.page = Page::Conversations;
}

/// Trim to `Option<String>` — an empty display name / bio is `None` on the wire
/// (linux's `non_empty`).
fn non_empty(s: &str) -> Option<String> {
    let t = s.trim();
    (!t.is_empty()).then(|| t.to_string())
}

// ── Network half ─────────────────────────────────────────────────────────────

/// The network half of a profile gesture — owns only `Arc`s + owned data, so it
/// crosses a `tokio::spawn` (agent path) or is awaited inline (keyboard path).
pub enum Op {
    /// A feed op the Posts tab issues — its window's reads and verbs, or the
    /// detail door on the timeline's manager ([`posts`]). Its outcome folds
    /// through the feed's own fold.
    Feed(Box<crate::feed::Op>),
    /// The private section's Save — the changed registers through the shared
    /// `contact_overlays::save_staged` answer ([`private`]).
    SavePrivate {
        manager: Arc<fauna_conversations::ConversationsManager>,
        store: fauna_sync_engine::account_runtime::AccountStoreHandle,
        actor: String,
        changes: fauna_core::contact_overlay::OverlayChanges,
    },
    /// `fauna.profile.get` → the SELF edit-form RMW base + editable-field seed,
    /// through the shared `load_profile_edit_base`: a succession link the base
    /// needs is proven and recorded in `accounts` before the form can save, so
    /// the save's registry read admits the base (`profile.md` § After an
    /// identity succession → *A successor device with no recorded succession
    /// link*).
    LoadEditBase {
        nest: Arc<NestClient>,
        secret: SecretArray32,
        accounts: fauna_client_accounts::AccountRegistry,
    },
    /// `build_edited_profile_with_images` (sign) → `fauna.profile.set` →
    /// refetch the header. A `StagedImage::Upload` path is uploaded via the
    /// shared public-post blob path first (`content` is required only then —
    /// `Keep`/`Clear` need no HTTP plane, matching a text-only save).
    Save {
        nest: Arc<NestClient>,
        content: Option<Arc<dyn NestContentApi>>,
        /// The refetched header's picture source (`None` before the feed's
        /// content plane exists — the pictures then stay unpainted).
        blobs: Option<fauna_client::PostBlobHosts>,
        secret: SecretArray32,
        self_actor_id: String,
        base_body: Option<Vec<u8>>,
        /// [`EditForm::last_direct_body`] — the owner's own word beside a
        /// delegated base.
        last_direct_body: Option<Vec<u8>>,
        /// The registry's predecessor ids for this identity, resolved at
        /// dispatch — what admits a base a succession moved onto it
        /// (`profile.md` § After an identity succession, the successor
        /// RE-PUBLISHES).
        predecessors: Vec<ActorId>,
        display_name: Option<String>,
        bio: Option<String>,
        links: Vec<ProfileLink>,
        avatar: StagedImage,
        banner: StagedImage,
        /// `profile-edit-audience-toggle`'s staged value (`None` = untouched).
        audience: Option<bool>,
    },
    /// `subscribe(author, "followers")` — across nests when the author's home
    /// is another nest (`SubscriptionsClient::subscribe_across_nests`).
    Follow {
        nest: Arc<NestClient>,
        secret: SecretArray32,
        author: String,
        home_nest_url: Option<String>,
    },
    /// `knocks_block` / `knocks_unblock`.
    ToggleBlock {
        nest: Arc<NestClient>,
        actor_id: String,
        currently_blocked: bool,
    },
    /// The knock — the contacts page's own send (`crate::contacts::send_knock`),
    /// so the two pages cannot classify the guardian refusal differently.
    SendKnock {
        nest: Arc<NestClient>,
        node_url: String,
        secret: [u8; 32],
        recipient: String,
        recipient_nest_url: Option<String>,
    },
    /// The web-origin read (`crate::settings::read_web_page`) the §4 return-URL
    /// display builds on — the same read the feed's ⋯ overflow hydrates, folded
    /// into the same `SettingsState`, so the surfaces cannot disagree about a
    /// creator's address.
    #[cfg(feature = "payments")]
    HydrateWebOrigin {
        nest: Arc<NestClient>,
        handle: String,
    },
    /// `fauna.family.contact.request` + the status re-read
    /// (`crate::contacts::ask_guardian`).
    AskGuardian { nest: Arc<NestClient>, peer: String },
    /// `offers.list(author)` + `status.get(author)`.
    LoadOffers {
        nest: Arc<NestClient>,
        author: String,
    },
    /// `subscribe(author, tier)` → refetch offers.
    Subscribe {
        nest: Arc<NestClient>,
        secret: SecretArray32,
        author: String,
        tier: String,
    },

    // ── SELF Tiers tab §§1–5 ────────────────────────────────────────────
    /// The whole tab's read ([`tiers::fetch_author`]).
    LoadAuthor {
        nest: Arc<NestClient>,
        roster_tier: String,
    },
    /// §3 only, for the roster tier-select change.
    LoadRoster { nest: Arc<NestClient>, tier: String },
    /// §1 create (`SubscriptionsAuthor::create_tier` — mints the period key and
    /// birth KeyBlob) or update (`tiers_update` — metadata only, no custody
    /// change), then re-read the tab.
    SaveTier {
        nest: Arc<NestClient>,
        secret: SecretArray32,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        roster_tier: String,
        form: tiers::TierForm,
    },
    /// §1 `tiers_delete` → re-read.
    DeleteTier {
        nest: Arc<NestClient>,
        roster_tier: String,
        name: String,
    },
    /// §2 `SubscriptionsAuthor::approve_subscriber` (mint+upload) → re-read.
    /// Boxed: `PendingRequest` carries a 1184-byte ML-KEM key, which would make
    /// this the enum's dominant variant otherwise.
    ApproveRequest {
        nest: Arc<NestClient>,
        secret: SecretArray32,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        roster_tier: String,
        request: Box<PendingRequest>,
    },
    /// §2 `requests_reject` → re-read.
    RejectRequest {
        nest: Arc<NestClient>,
        roster_tier: String,
        request_id: i64,
    },
    /// §3 `SubscriptionsAuthor::remove_subscriber` (roster rotation) → re-read.
    RemoveSubscriber {
        nest: Arc<NestClient>,
        secret: SecretArray32,
        period_keys: fauna_client_subscriptions::SharedPeriodKeyStore,
        tier: String,
        subscriber: ActorId,
    },
    /// §4 `providers_set` → re-read.
    #[cfg(feature = "payments")]
    SaveProvider {
        nest: Arc<NestClient>,
        roster_tier: String,
        form: tiers::ProviderForm,
    },
    /// §4 `providers_remove` → re-read.
    #[cfg(feature = "payments")]
    RemoveProvider {
        nest: Arc<NestClient>,
        roster_tier: String,
        kind: String,
    },
    /// §5 `claims_mint` → re-read.
    #[cfg(feature = "payments")]
    MintClaim {
        nest: Arc<NestClient>,
        roster_tier: String,
        tier: String,
    },
    /// §5 `claims_void` → re-read.
    #[cfg(feature = "payments")]
    VoidClaim {
        nest: Arc<NestClient>,
        roster_tier: String,
        code: String,
    },
}

/// What a [`Op`] (or an open-time fetch) resolved to; folded back by
/// [`apply_outcome`].
#[derive(Debug)]
pub enum Outcome {
    /// The published display name (`None` = not_found / blank → fall back).
    /// An open-time read, so it carries the [`ProfileState::open_epoch`] it was
    /// issued under and is dropped if the page has moved on. `actor_home` is
    /// the same read's [`ProfileState::actor_home`] (always `None` for SELF).
    HeaderName {
        epoch: u64,
        name: Option<String>,
        actor_home: Option<String>,
        /// The pictures the same read names ([`ProfileState::pictures`]).
        pictures: HeaderPictures,
    },
    /// The header pictures' art ([`fetch_pictures`]), with the open epoch the
    /// read was issued under — a stale answer is dropped like the name's.
    Pictures { epoch: u64, art: Box<HeaderArt> },
    /// The edit-form RMW base + the seeded editable fields.
    EditBase {
        base_body: Option<Vec<u8>>,
        /// [`EditForm::last_direct_body`].
        last_direct_body: Option<Vec<u8>>,
        display_name: Option<String>,
        bio: Option<String>,
        links: Vec<ProfileLink>,
        /// Whether the base is open to other nests (the toggle's start), as
        /// the save will sign it.
        audience_everyone: bool,
    },
    /// A publish landed — close the form and re-render the refetched header,
    /// its name and its pictures (the save may have changed either).
    SaveDone {
        name: Option<String>,
        pictures: HeaderPictures,
        art: Box<HeaderArt>,
    },
    /// The follow subscribe landed (label → Following).
    Followed,
    /// The initial OTHER block-toggle state (a background open-time read), with
    /// the [`ProfileState::open_epoch`] it was issued under.
    ///
    /// **Hydration only.** It describes the edge as `contacts.list` saw it at
    /// open, so it is authoritative right up until something newer authors the
    /// same field — a re-open (a different epoch) or the viewer's own press
    /// ([`ProfileState::block_user_authored`]). Applying it unconditionally is
    /// what made the Block⇄Unblock toggle look one-way: the read is issued at
    /// open and answers a full WS round-trip later, routinely *after* the first
    /// press, so it rewound `is_blocked` to not-blocked and the next press
    /// re-blocked — succeeding on the wire, leaving the label on "Unblock" with
    /// no error to show for it.
    BlockState { epoch: u64, blocked: bool },
    /// The block toggle flipped to this new state.
    BlockToggled(bool),
    /// How the knock to `peer` ended. Carries the peer because the reply can
    /// outlive the open it was sent from — a "Sent" or a guardian refusal for
    /// the actor you just left must not paint on the one you are now viewing.
    Knock {
        peer: String,
        result: crate::contacts::KnockSend,
    },
    /// The guardian ask for `peer` landed; `requests` is the ward's re-read
    /// `status.contact_requests` (empty on a failed re-read).
    ContactRequested {
        peer: String,
        requests: Vec<fauna_client_family::family::FamilyContactRequestInfo>,
    },
    /// The ask itself failed (cap reached, peer blocked, knob off — typed
    /// refusals the ward reads verbatim).
    ContactRequestFailed(String),
    /// A fresh offers list + the viewer's held tier.
    Offers {
        offers: Vec<TierItem>,
        status_tier: Option<String>,
    },
    /// A subscribe landed; carries the refetched offers/status + whether the
    /// nest queued it (encrypted mode) so the transient pending flag can settle.
    Subscribed {
        offers: Vec<TierItem>,
        status_tier: Option<String>,
        tier: String,
        queued: bool,
    },
    /// A fresh read of the SELF Tiers tab §§1–5. Every author mutation resolves
    /// to this (the page is observer-free: mutate, then re-read authoritatively),
    /// so there is one fold path for all of §§1–5.
    Author(Box<tiers::AuthorSnapshot>),
    /// §3 alone, after a roster tier-select change.
    Roster(Vec<SubscriberEntry>),
    /// A transport/build failure — lands on the page's `error-message`.
    Failed(String),
    /// A Tiers-tab mutation failed. Distinct from [`Self::Failed`] only in that
    /// it also clears [`tiers::AuthorState::busy`] — an approve that errors must
    /// not leave `subscription-request-busy` painted forever.
    AuthorFailed(String),
    /// The private section's Save for `actor` landed (the projection already
    /// reloaded) — drop the staging.
    PrivateSaved { actor: String },
    /// A [`Op::Feed`] landed — the feed's fold applies it.
    Feed(Box<crate::feed::Outcome>),
    /// The web-origin read the §4 return-URL display builds on (`None` = the
    /// read failed or the nest serves no web content: the display then says
    /// the creator has no public address yet, which is already true of them,
    /// so no error is raised for it).
    #[cfg(feature = "payments")]
    WebOrigin(Option<crate::settings::WebPageRead>),
}

impl Op {
    pub async fn run(self) -> Outcome {
        match self {
            Op::Feed(op) => Outcome::Feed(Box::new(Box::pin(op.run()).await)),
            Op::SavePrivate {
                manager,
                store,
                actor,
                changes,
            } => private::save(manager, store, actor, changes).await,
            Op::LoadEditBase {
                nest,
                secret,
                accounts,
            } => load_edit_base(nest, &secret, &accounts).await,
            Op::Save {
                nest,
                content,
                blobs,
                secret,
                self_actor_id,
                base_body,
                last_direct_body,
                predecessors,
                display_name,
                bio,
                links,
                avatar,
                banner,
                audience,
            } => {
                let avatar = match resolve_staged_image(avatar, content.as_deref()).await {
                    Ok(edit) => edit,
                    Err(e) => return Outcome::Failed(format!("avatar: {e}")),
                };
                let banner = match resolve_staged_image(banner, content.as_deref()).await {
                    Ok(edit) => edit,
                    Err(e) => return Outcome::Failed(format!("banner: {e}")),
                };
                let kp = ActorKeypair::from_secret(secret.to_array());
                let body = match build_edited_profile_with_images(
                    &kp,
                    base_body.as_deref(),
                    last_direct_body.as_deref(),
                    &predecessors,
                    display_name,
                    bio,
                    links,
                    avatar,
                    banner,
                    audience.map(fauna_client_profile::audience_from_toggle),
                ) {
                    Ok(b) => b,
                    Err(e) => return Outcome::Failed(format!("build profile: {e}")),
                };
                if let Err(e) = ProfileClient::new(Arc::clone(&nest))
                    .profile_set(body)
                    .await
                {
                    return Outcome::Failed(format!("publish profile: {e}"));
                }
                let HeaderFetch { name, pictures, .. } =
                    fetch_header_profile(nest, &self_actor_id, None).await;
                let art = match &blobs {
                    Some(blobs) => fetch_pictures(blobs, &pictures).await,
                    None => HeaderArt::default(),
                };
                Outcome::SaveDone {
                    name,
                    pictures,
                    art: Box::new(art),
                }
            }
            Op::Follow {
                nest,
                secret,
                author,
                home_nest_url,
            } => {
                let Ok(author_id) = ActorId::from_hex(&author) else {
                    return Outcome::Failed("follow: malformed actor id".into());
                };
                let kp = ActorKeypair::from_secret(secret.to_array());
                match SubscriptionsClient::new(nest)
                    .subscribe_across_nests(author_id, FOLLOWERS_TIER, &kp, home_nest_url)
                    .await
                {
                    Ok(_) => Outcome::Followed,
                    Err(e) => Outcome::Failed(format!("follow: {e}")),
                }
            }
            Op::ToggleBlock {
                nest,
                actor_id,
                currently_blocked,
            } => {
                let client = ContactsClient::new(nest);
                let result = if currently_blocked {
                    client.knocks_unblock(actor_id).await.map(|_| ())
                } else {
                    client.knocks_block(actor_id).await.map(|_| ())
                };
                match result {
                    Ok(()) => Outcome::BlockToggled(!currently_blocked),
                    Err(e) => Outcome::Failed(format!("block: {e}")),
                }
            }
            Op::SendKnock {
                nest,
                node_url,
                secret,
                recipient,
                recipient_nest_url,
            } => {
                let result = crate::contacts::send_knock(
                    nest,
                    &node_url,
                    secret,
                    recipient.clone(),
                    recipient_nest_url,
                )
                .await;
                Outcome::Knock {
                    peer: recipient,
                    result,
                }
            }
            #[cfg(feature = "payments")]
            Op::HydrateWebOrigin { nest, handle } => {
                let client = fauna_client_web::WebClient::new(nest);
                Outcome::WebOrigin(crate::settings::read_web_page(&client, &handle).await.ok())
            }
            Op::AskGuardian { nest, peer } => {
                match crate::contacts::ask_guardian(nest, &peer).await {
                    Ok(requests) => Outcome::ContactRequested { peer, requests },
                    Err(e) => Outcome::ContactRequestFailed(e),
                }
            }
            Op::LoadOffers { nest, author } => match fetch_offers(&nest, &author).await {
                Ok((offers, status_tier)) => Outcome::Offers {
                    offers,
                    status_tier,
                },
                Err(e) => Outcome::Failed(e),
            },
            Op::Subscribe {
                nest,
                secret,
                author,
                tier,
            } => {
                let Ok(author_id) = ActorId::from_hex(&author) else {
                    return Outcome::Failed("subscribe: malformed actor id".into());
                };
                let kp = ActorKeypair::from_secret(secret.to_array());
                let queued = match SubscriptionsClient::new(Arc::clone(&nest))
                    .subscribe_publishing_ek(author_id, tier.clone(), &kp)
                    .await
                {
                    Ok(SubscribeReply::Approved { .. }) => false,
                    // An outcome a newer nest added: sent, state unknown — the
                    // re-read below decides what to show, never a guess.
                    Ok(SubscribeReply::Queued { .. } | SubscribeReply::Unknown) => true,
                    Err(e) => return Outcome::Failed(format!("subscribe: {e}")),
                };
                // Re-read authoritatively (linux's refresh-after-subscribe).
                match fetch_offers(&nest, &author).await {
                    Ok((offers, status_tier)) => Outcome::Subscribed {
                        offers,
                        status_tier,
                        tier,
                        queued,
                    },
                    Err(e) => Outcome::Failed(e),
                }
            }

            // ── SELF Tiers tab §§1–5 ────────────────────────────────────
            Op::LoadAuthor { nest, roster_tier } => author_snapshot(&nest, &roster_tier).await,
            Op::LoadRoster { nest, tier } => match tiers::fetch_roster(&nest, &tier).await {
                Ok(subscribers) => Outcome::Roster(subscribers),
                Err(e) => Outcome::AuthorFailed(e),
            },
            Op::SaveTier {
                nest,
                secret,
                period_keys,
                roster_tier,
                form,
            } => {
                // Shared non-negative parse (value-formatting.md § Tier rank);
                // blank/garbage falls to rank 0, matching every app's form.
                let rank = fauna_core::format::parse_count(&form.rank).unwrap_or(0);
                let name = form.name.trim().to_string();
                if name.is_empty() {
                    return Outcome::AuthorFailed("tier name is required".into());
                }
                let description = non_empty(&form.description);
                let price_hint = non_empty(&form.price_hint);
                let payment_url = non_empty(&form.payment_url);
                // Same shape as the three fields above (and their shared,
                // separately-tracked "no clear verb yet" gap —
                // monetization.md § The asking price → Editability): a parsed
                // sats value on the wire, `None` for empty OR unparseable.
                // `TierAskingPrice::from_sats` owns the sats→msat arithmetic;
                // no app writes the multiply itself.
                let asking_price = non_empty(&form.asking_price)
                    .and_then(|s| s.parse::<u64>().ok())
                    .and_then(fauna_protocol::subscriptions::TierAskingPrice::from_sats);
                let result = match &form.editing {
                    // Update: metadata only. `tiers_update` touches no custody
                    // state, so it goes straight through the plain client rather
                    // than the author orchestrator.
                    // Every field is `Some` — the form was seeded from the row,
                    // so its buffers ARE the intended post-edit values; passing
                    // `None` would mean "keep current" and silently discard a
                    // field the user just cleared.
                    Some(original) => SubscriptionsClient::new(Arc::clone(&nest))
                        .tiers_update(
                            original.clone(),
                            Some(rank),
                            description,
                            price_hint,
                            payment_url,
                            Some(form.auto_approve),
                            asking_price,
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("update tier: {e}")),
                    // Create: mints + persists the period key and uploads the
                    // birth KeyBlob (shared orchestration — never open-coded).
                    None => tiers::author(Arc::clone(&nest), &secret, period_keys)
                        .create_tier(
                            &name,
                            rank,
                            description,
                            price_hint,
                            payment_url,
                            form.auto_approve,
                            // Ordinary management form — never a per-post
                            // pay-to-unlock tier; that create-time-immutable
                            // designation is set only by the "sell this post"
                            // orchestration (`monetization.md` § Per-post
                            // pay-to-unlock).
                            None,
                            asking_price,
                            // The tier-management form mints OFFERED tiers; the
                            // reserved hidden tier is provisioned by the
                            // archive-import machine, not by hand.
                            false,
                        )
                        .await
                        .map(|_| ())
                        .map_err(|e| format!("create tier: {e}")),
                };
                match result {
                    Ok(()) => author_snapshot(&nest, &roster_tier).await,
                    Err(e) => Outcome::AuthorFailed(e),
                }
            }
            Op::DeleteTier {
                nest,
                roster_tier,
                name,
            } => match SubscriptionsClient::new(Arc::clone(&nest))
                .tiers_delete(name)
                .await
            {
                Ok(_) => author_snapshot(&nest, &roster_tier).await,
                Err(e) => Outcome::AuthorFailed(format!("delete tier: {e}")),
            },
            Op::ApproveRequest {
                nest,
                secret,
                period_keys,
                roster_tier,
                request,
            } => {
                match tiers::author(Arc::clone(&nest), &secret, period_keys)
                    .approve_subscriber(&request)
                    .await
                {
                    Ok(_) => author_snapshot(&nest, &roster_tier).await,
                    Err(e) => Outcome::AuthorFailed(format!("approve: {e}")),
                }
            }
            Op::RejectRequest {
                nest,
                roster_tier,
                request_id,
            } => match SubscriptionsClient::new(Arc::clone(&nest))
                .requests_reject(request_id)
                .await
            {
                Ok(_) => author_snapshot(&nest, &roster_tier).await,
                Err(e) => Outcome::AuthorFailed(format!("reject: {e}")),
            },
            Op::RemoveSubscriber {
                nest,
                secret,
                period_keys,
                tier,
                subscriber,
            } => {
                match tiers::author(Arc::clone(&nest), &secret, period_keys)
                    .remove_subscriber(&tier, subscriber)
                    .await
                {
                    Ok(()) => author_snapshot(&nest, &tier).await,
                    Err(e) => Outcome::AuthorFailed(format!("remove subscriber: {e}")),
                }
            }
            #[cfg(feature = "payments")]
            Op::SaveProvider {
                nest,
                roster_tier,
                form,
            } => {
                // No client-side validation: the nest rejects unknown kinds,
                // dangling tiers and empty secrets with typed `fauna.payments.*`
                // errors that surface on the shared error label (linux's shape).
                match PaymentsClient::new(Arc::clone(&nest))
                    .providers_set(form.kind, form.secret, form.tier)
                    .await
                {
                    Ok(_) => author_snapshot(&nest, &roster_tier).await,
                    Err(e) => Outcome::AuthorFailed(format!("save provider: {e}")),
                }
            }
            #[cfg(feature = "payments")]
            Op::RemoveProvider {
                nest,
                roster_tier,
                kind,
            } => match PaymentsClient::new(Arc::clone(&nest))
                .providers_remove(kind)
                .await
            {
                Ok(_) => author_snapshot(&nest, &roster_tier).await,
                Err(e) => Outcome::AuthorFailed(format!("remove provider: {e}")),
            },
            #[cfg(feature = "payments")]
            Op::MintClaim {
                nest,
                roster_tier,
                tier,
            } => match PaymentsClient::new(Arc::clone(&nest))
                .claims_mint(tier, None)
                .await
            {
                Ok(_) => author_snapshot(&nest, &roster_tier).await,
                Err(e) => Outcome::AuthorFailed(format!("mint claim: {e}")),
            },
            #[cfg(feature = "payments")]
            Op::VoidClaim {
                nest,
                roster_tier,
                code,
            } => match PaymentsClient::new(Arc::clone(&nest))
                .claims_void(code)
                .await
            {
                Ok(_) => author_snapshot(&nest, &roster_tier).await,
                // The row the author pressed is no longer theirs to void (gone,
                // or never theirs): the list is what is stale, so re-read it
                // rather than surface an error about a row that should not be
                // painted.
                Err(fauna_client::NestClientError::Rpc(ref e))
                    if e.code == "fauna.payments.claim_not_found" =>
                {
                    author_snapshot(&nest, &roster_tier).await
                }
                Err(e) => Outcome::AuthorFailed(format!("void claim: {e}")),
            },
        }
    }
}

/// Re-read §§1–5 into an [`Outcome`] — the tail of every author mutation (the
/// page is observer-free, so a mutation's truth comes from the re-read, never
/// from patching local state).
async fn author_snapshot(nest: &Arc<NestClient>, roster_tier: &str) -> Outcome {
    match tiers::fetch_author(nest, roster_tier).await {
        Ok(snapshot) => Outcome::Author(Box::new(snapshot)),
        Err(e) => Outcome::AuthorFailed(e),
    }
}

/// Resolve one staged image field into the [`ProfileImageEdit`]
/// `build_edited_profile_with_images` needs. `Keep`/`Clear` need no HTTP plane
/// (matching a text-only save's cost); `Upload` uploads the staged path via the
/// shared public-post blob path (`media.md` § Encryption at rest: avatar/banner
/// blobs are "the same shape" as post attachments, so this is the identical
/// mechanism feed's `submit_post` uses, not a new one) — mirroring feed's rule
/// that a save must never silently drop a picture the user picked: an upload
/// failure fails the whole save rather than publishing text-only.
async fn resolve_staged_image(
    staged: StagedImage,
    content: Option<&dyn NestContentApi>,
) -> Result<ProfileImageEdit, String> {
    match staged {
        StagedImage::Keep => Ok(ProfileImageEdit::Keep),
        StagedImage::Clear => Ok(ProfileImageEdit::Clear),
        StagedImage::Upload(path) => {
            let content = content.ok_or_else(|| "no HTTP/bulk plane (pre-auth)".to_string())?;
            let blob = upload_public_post_blob(content, &path).await?;
            ProfileImageEdit::set_from_hex(&blob.blob_hash).map_err(|e| e.to_string())
        }
    }
}

/// The header-identity read (`profile.md` § State & data shape) → the raw
/// profile bytes plus the published, non-blank display name. This nest first;
/// for an OTHER actor with a `home` in hand, its `not_found` falls through to
/// the relay (`profile.md` § Where logic lives → *The remote read*, the shared
/// `ProfileClient::profile_get_or_relay`). Any failure of either leg is the
/// header's actor-id fallback — `(None, None)`, nothing on `error-message`.
/// The bytes ride back so an OTHER-profile open resolves the actor's home from
/// and harvests the document it just rendered (the page-path peer-anchor arm)
/// without a second fetch.
///
/// The pictures the profile names ride back too, with the nest that answered
/// as their source (the shared `ProfileClient::profile_header_read`): this
/// nest's blob route for a profile it served, the home's for a relayed one.
async fn fetch_header_profile<R>(nest: R, actor_id: &str, home: Option<&str>) -> HeaderFetch
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    let Ok(read) = ProfileClient::new(nest)
        .profile_header_read(actor_id.to_string(), home)
        .await
    else {
        return HeaderFetch::default();
    };
    let bytes = read.reply.body.to_vec();
    let display = decode_profile_display(&bytes).ok();
    let name = display
        .as_ref()
        .and_then(|d| d.display_name.clone())
        .filter(|n| !n.trim().is_empty());
    let pictures = display
        .map(|d| HeaderPictures {
            avatar: d.avatar_hash_hex(),
            banner: d.banner_hash_hex(),
            home: read.relayed_from.map(|url| {
                fauna_client::ForeignHome::new(&url, read.home_nest_actor_id.as_deref())
            }),
        })
        .unwrap_or_default();
    HeaderFetch {
        name,
        bytes: Some(bytes),
        pictures,
    }
}

/// What [`fetch_header_profile`] read: the published, non-blank display name,
/// the profile bytes, and the pictures the profile names. All empty on any
/// failure — the header's actor-id fallback.
#[derive(Debug, Default, PartialEq, Eq)]
struct HeaderFetch {
    name: Option<String>,
    bytes: Option<Vec<u8>>,
    pictures: HeaderPictures,
}

/// The header's stored pictures (`profile-avatar`, `profile-banner`): the hex
/// blob hashes the profile names, and the nest whose blob route serves them —
/// `None` for this nest, the home a relayed profile came from otherwise
/// (`profile.md` § Where logic lives → *The remote read*, *Bytes direct, as
/// for a relayed post*). That home is reached anonymously through
/// `PostBlobHosts`, never through this nest, its pin graduated against the
/// identity the relay reply stamped (`HeaderRead::home_nest_actor_id`) — the
/// post arm's plane, so a home serving the self-signed floor paints; absent a
/// stamp the plane keeps the WebPKI floor (`transport-trust.md` § The post arm
/// → *A relayed profile's pictures*).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderPictures {
    avatar: Option<String>,
    banner: Option<String>,
    home: Option<fauna_client::ForeignHome>,
}

impl HeaderPictures {
    fn is_empty(&self) -> bool {
        self.avatar.is_none() && self.banner.is_none()
    }
}

/// The header pictures' art — `None` while loading, after a failed fetch, or
/// for bytes that will not rasterize; the element then paints empty.
#[derive(Debug, Clone, Default)]
pub struct HeaderArt {
    avatar: Option<crate::thumbnail::Thumbnail>,
    banner: Option<crate::thumbnail::Thumbnail>,
}

/// Fetch and rasterize the header's pictures through the session's host pick:
/// capped and verified against the hash the profile names
/// ([`fauna_client::PostBlobHosts::get`]). Any failure is that picture's
/// empty element, never `error-message`.
async fn fetch_pictures(
    blobs: &fauna_client::PostBlobHosts,
    pictures: &HeaderPictures,
) -> HeaderArt {
    let picture = |hash: Option<String>, cols: u32| async move {
        let bytes = blobs.get(pictures.home.as_ref(), &hash?).await.ok()?;
        crate::thumbnail::rasterize(&bytes, cols)
    };
    let (avatar, banner) = futures_util::future::join(
        picture(pictures.avatar.clone(), crate::thumbnail::THUMBNAIL_COLS),
        picture(pictures.banner.clone(), crate::thumbnail::POST_IMAGE_COLS),
    )
    .await;
    HeaderArt { avatar, banner }
}

/// The OTHER-profile open's **page-path peer-anchor harvest** — harvest rule
/// 4's "on the profile page" arm (`identity-succession.md` § the peer-profile
/// harvest): the verified bytes the page just fetched to render are seeded
/// through the one-door gate, costing no second fetch, and a fresh seed
/// re-drives parked succession statements exactly as the roster sweep does
/// (the announcement is what keeps the witness's once-per-seed-generation
/// anchor read fresh). Factored off the spawn so a test can drive it with a
/// fake store and no nest.
async fn harvest_page_read(
    peer: &fauna_core::identity::ActorId,
    bytes: &[u8],
    store: &dyn fauna_conversations::backend::PeerAnchorStore,
    conversations: Option<std::sync::Weak<fauna_conversations::ConversationsSession>>,
) {
    let outcome = fauna_client_recovery::harvest::seed_profile_bytes(peer, bytes, store).await;
    if matches!(
        outcome,
        fauna_client_recovery::harvest::HarvestOutcome::Seeded(_)
    ) && let Some(session) = conversations.and_then(|w| w.upgrade())
    {
        let repointed = session.redrive_parked_successions(peer).await;
        if repointed > 0 {
            tracing::debug!(
                actor = %peer.to_hex(),
                repointed,
                "profile-page harvest re-drive settled parked succession statements"
            );
        }
    }
}

/// The SELF profile fetch for the edit-form RMW base + the editable-field seed
/// (`not_found` ⇒ first publish: empty form, `None` base). The toggle's start
/// comes from the shared own-form projection over the base and the owner's
/// last identity-signed row served beside it — the resolver the save's
/// admission runs, so the form shows exactly what an untouched save signs
/// (`profile.md` § Where logic lives → *Profile publish/edit* → *Edit
/// surface*).
async fn load_edit_base<R>(
    nest: R,
    secret: &SecretArray32,
    accounts: &fauna_client_accounts::AccountRegistry,
) -> Outcome
where
    R: fauna_protocol::RpcRequester,
    R::Error: fauna_protocol::RpcErrorClass,
{
    let kp = ActorKeypair::from_secret(secret.to_array());
    match fauna_client_recovery::ceremony::load_profile_edit_base(nest, accounts, &kp).await {
        Ok(None) => empty_edit_base(),
        Ok(Some(base)) => {
            match fauna_client_profile::decode_own_profile_display(
                &kp.actor_id(),
                &base.body,
                base.last_direct_body.as_deref(),
            ) {
                Ok(d) => Outcome::EditBase {
                    base_body: Some(base.body),
                    last_direct_body: base.last_direct_body,
                    display_name: d.display_name,
                    bio: d.bio,
                    links: d.links,
                    audience_everyone: d.audience.is_everyone(),
                },
                // A stored profile that won't decode: treat as first publish
                // rather than block editing on a corrupt row.
                Err(_) => empty_edit_base(),
            }
        }
        Err(_) => empty_edit_base(),
    }
}

fn empty_edit_base() -> Outcome {
    Outcome::EditBase {
        base_body: None,
        last_direct_body: None,
        display_name: None,
        bio: None,
        links: vec![],
        audience_everyone: false,
    }
}

/// Whether the viewer's roster currently blocks `actor_id` — the initial
/// Block⇄Unblock toggle state (`contact_row_blocks_actor` folded over the roster).
async fn fetch_block_state(nest: &Arc<NestClient>, actor_id: &str) -> bool {
    match ContactsClient::new(Arc::clone(nest)).contacts_list().await {
        Ok(reply) => reply
            .contacts
            .iter()
            .any(|c| format::contact_row_blocks_actor(&c.peer_id, &c.status, actor_id)),
        Err(_) => false,
    }
}

/// `fauna.subscriptions.offers.list` + `status.get` — the OTHER-profile offers
/// browse reads. A failed status read is non-fatal (the viewer may hold no
/// subscription), so it degrades to `None` rather than failing the whole load.
async fn fetch_offers(
    nest: &Arc<NestClient>,
    author: &str,
) -> Result<(Vec<TierItem>, Option<String>), String> {
    let author_id = ActorId::from_hex(author).map_err(|e| format!("offers: bad actor id: {e}"))?;
    let client = SubscriptionsClient::new(Arc::clone(nest));
    let offers = client
        .offers_list(author_id)
        .await
        .map_err(|e| format!("load offers: {e}"))?;
    let status_tier = client.status_get(author_id).await.ok().and_then(|r| r.tier);
    Ok((offers, status_tier))
}

/// Fold an outcome back into the page — one function for both dispatch paths.
pub fn apply_outcome(app: &mut App, outcome: Outcome) {
    let st = &mut app.profile;
    match outcome {
        // An open-time read: it answers the actor that open was showing, so a
        // reply that outlived its open belongs to nobody on screen.
        Outcome::HeaderName {
            epoch,
            name,
            actor_home,
            pictures,
        } => {
            if epoch == st.open_epoch {
                st.header_name = name;
                st.actor_home = actor_home;
                st.pictures = pictures;
            }
        }
        Outcome::Pictures { epoch, art } => {
            if epoch == st.open_epoch {
                st.art = *art;
            }
        }
        Outcome::EditBase {
            base_body,
            last_direct_body,
            display_name,
            bio,
            links,
            audience_everyone,
        } => {
            if let Some(edit) = &mut st.edit {
                edit.base_body = base_body;
                edit.last_direct_body = last_direct_body;
                // The form opens before its base lands, so a press may already
                // be staged: it stands, unless it now matches the base.
                edit.audience_base = audience_everyone;
                if edit.audience_edit == Some(audience_everyone) {
                    edit.audience_edit = None;
                }
                edit.display_name = display_name.unwrap_or_default();
                edit.bio = bio.unwrap_or_default();
                edit.links = links.into_iter().map(|l| (l.label, l.uri)).collect();
            }
        }
        Outcome::SaveDone {
            name,
            pictures,
            art,
        } => {
            st.edit = None;
            st.header_name = name;
            st.pictures = pictures;
            st.art = *art;
            app.errors.remove(&Page::Profile);
        }
        Outcome::Followed => {
            st.followed = true;
            app.errors.remove(&Page::Profile);
        }
        // A background open-time read — never clears a live error, and never
        // outranks something newer that authored the same field: a later open
        // (a different epoch) or the viewer's own press, which is the truth the
        // read was too early to see.
        Outcome::BlockState { epoch, blocked } => {
            if epoch == st.open_epoch && !st.block_user_authored {
                st.is_blocked = Some(blocked);
            }
        }
        Outcome::BlockToggled(blocked) => {
            st.is_blocked = Some(blocked);
            st.block_user_authored = true;
            app.errors.remove(&Page::Profile);
        }
        // Whatever the page now shows, the knock went to `peer`: only its own
        // open may paint "Sent" or offer the ask.
        Outcome::Knock { peer, result } => {
            let current = st.viewing.as_deref() == Some(peer.as_str());
            match result {
                crate::contacts::KnockSend::Sent => {
                    if current {
                        st.knock_sent = true;
                    }
                    app.errors.remove(&Page::Profile);
                }
                crate::contacts::KnockSend::RefusedByGuardian => {
                    if current {
                        st.guardian_refused = true;
                    }
                    // Still a real error — the knock did not happen — just no
                    // longer a dead end: the ask button paints beside it.
                    app.errors
                        .insert(Page::Profile, c::GUARDIAN_APPROVAL_REQUIRED.to_string());
                }
                crate::contacts::KnockSend::Failed(e) => {
                    app.errors.insert(Page::Profile, e);
                }
            }
        }
        Outcome::ContactRequested { peer, requests } => {
            if st.viewing.as_deref() == Some(peer.as_str()) {
                st.contact_ask_sent = true;
            }
            // The nest's own list is the durable truth for every page; an empty
            // one means the re-read failed, and dropping what the client
            // already holds on that would be strictly worse than keeping it.
            if !requests.is_empty() {
                app.family.own_contact_requests = requests;
            }
            app.errors.remove(&Page::Profile);
        }
        Outcome::ContactRequestFailed(e) => {
            app.errors.insert(Page::Profile, e);
        }
        Outcome::Offers {
            offers,
            status_tier,
        } => {
            st.offers = offers;
            st.status_tier = status_tier;
            app.errors.remove(&Page::Profile);
        }
        Outcome::Subscribed {
            offers,
            status_tier,
            tier,
            queued,
        } => {
            st.offers = offers;
            st.status_tier = status_tier;
            // Auto-approve/plaintext ⇒ Approved ⇒ status_tier now names the
            // tier ⇒ the transient is spent; encrypted ⇒ Queued ⇒ keep it.
            st.pending_tier = queued.then_some(tier);
            app.errors.remove(&Page::Profile);
        }
        Outcome::Author(snapshot) => {
            let tiers::AuthorSnapshot {
                tiers,
                requests,
                subscribers,
                roster_tier,
                #[cfg(feature = "payments")]
                providers,
                #[cfg(feature = "payments")]
                claims,
            } = *snapshot;
            // A mutation's re-read always closes whichever form drove it — a
            // still-open form over fresh rows is the "did my save land?"
            // ambiguity the linux view avoids the same way.
            st.author.form = None;
            #[cfg(feature = "payments")]
            {
                st.author.provider_form = None;
                // Keep the §5 pick if it survived, else fall to the first tier,
                // so the mint button is never armed against a deleted tier —
                // the same rule §3's roster follows.
                st.author.claim_tier = tiers::pick_tier(&tiers, &st.author.claim_tier);
                // A fresh list is a fresh decision: no confirm armed over
                // rows the author has not seen yet.
                st.author.claim_void_armed = None;
            }
            st.author.busy = false;
            st.author.tiers = tiers;
            st.author.requests = requests;
            st.author.subscribers = subscribers;
            st.author.roster_tier = roster_tier;
            #[cfg(feature = "payments")]
            {
                st.author.providers = providers;
                st.author.claims = claims;
            }
            app.errors.remove(&Page::Profile);
        }
        Outcome::Roster(subscribers) => {
            st.author.subscribers = subscribers;
            app.errors.remove(&Page::Profile);
        }
        Outcome::Failed(e) => {
            app.errors.insert(Page::Profile, e);
        }
        Outcome::PrivateSaved { actor } => private::apply_saved(app, &actor),
        Outcome::Feed(outcome) => crate::feed::apply_outcome(app, *outcome),
        #[cfg(feature = "payments")]
        Outcome::WebOrigin(read) => {
            if let Some(read) = read {
                app.settings.apply_web_page_read(read);
            }
        }
        Outcome::AuthorFailed(e) => {
            // Clear the in-flight marker too — an approve that errors must not
            // leave `subscription-request-busy` painted forever.
            st.author.busy = false;
            app.errors.insert(Page::Profile, e);
        }
    }
}

// ── Elements ─────────────────────────────────────────────────────────────────

/// The header identity: SELF = published display name → live display handle
/// (`<handle>@<current primary>`) → actor id; OTHER = the one shared resolver (`format::peer_display_label`,
/// value-formatting.md § Peer display label) over the viewer's own nickname,
/// the published display name, then the canonical short id (the OTHER header
/// has no cached handle). `public` is the name a nickname replaced — the
/// `profile-public-name` line beneath it.
fn header_label(st: &ProfileState) -> format::PeerLabel {
    if st.is_self() {
        let primary = if let Some(name) = st.header_name.as_ref().filter(|n| !n.trim().is_empty()) {
            name.clone()
        } else if let Some(address) = st.self_address.as_ref().filter(|a| !a.trim().is_empty()) {
            address.clone()
        } else {
            st.actor_id().to_string()
        };
        return format::PeerLabel {
            primary,
            public: None,
        };
    }
    format::peer_display_label(
        st.viewed_nickname().as_deref(),
        st.header_name.as_deref(),
        None,
        st.actor_id(),
    )
}

/// The Block⇄Unblock toggle label (`contact_toggle_block_label`) — tui is in the
/// flipped set (Block ⇄ Unblock), not the block-only interim.
fn block_label(st: &ProfileState) -> String {
    crate::wizard::localized(&format::contact_toggle_block_label(
        st.is_blocked.unwrap_or(false),
    ))
}

/// The per-tier offer-status badge text (`offer_status` → `offer_status_label`).
fn offer_status_display(st: &ProfileState, tier_name: &str) -> String {
    let pending = st.pending_tier.as_deref() == Some(tier_name);
    let status = format::offer_status(tier_name, st.status_tier.as_deref(), pending);
    crate::wizard::localized(&format::offer_status_label(status))
}

/// A header picture: its art once fetched, else empty (the empty-text
/// convention) — loading and a failed fetch look the same, and neither is an
/// error. The painted half-block `▀` text is the e2e paint observable, as for
/// the feed's `post-image`.
fn picture_element(id: &'static str, art: Option<&crate::thumbnail::Thumbnail>) -> Element {
    match art {
        Some(art) => Element::thumbnail(id, art.clone()),
        None => Element::label(id, " "),
    }
}

/// `profile-actor-id-copy-btn`, carrying the id it last copied as its `copied`
/// attr once it has fired (the settings web-link buttons' contract).
fn copy_id_button(st: &ProfileState) -> Element {
    let el = Element::gesture_button(
        ids::PROFILE_ACTOR_ID_COPY_BTN,
        t::COPY_ID,
        true,
        Gesture::Profile(Action::Copy),
    );
    match &st.copied {
        Some(id) => el.attr("copied", id.clone()),
        None => el,
    }
}

/// The profile page as one ordered element list (paint = registry = focus ring),
/// branching on `is_self`. The offer rows are FLAT indexed ids (one per row in
/// registration order), like the conversations bubble children — read by `[i]`,
/// never a positional scope.
pub fn elements(app: &App) -> Vec<Element> {
    let st = &app.profile;
    let header = header_label(st);
    let mut out = vec![
        // The page landmark (ui.yaml `profile-view`) — empty-text convention like
        // `contacts-view` / `conversations-view`.
        Element::label(ids::PROFILE_VIEW, " "),
        Element::label(ids::PAGE_HEADING, t::POSTS),
    ];
    // The header's stored pictures — present only when the profile names one.
    for (id, hash, art) in [
        (ids::PROFILE_BANNER, &st.pictures.banner, &st.art.banner),
        (ids::PROFILE_AVATAR, &st.pictures.avatar, &st.art.avatar),
    ] {
        if hash.is_some() {
            out.push(picture_element(id, art.as_ref()));
        }
    }
    out.push(Element::label(ids::PROFILE_HANDLE, header.primary));
    // A nickname never hides who this is (contacts.md § The private overlay,
    // guard 1): the public name it replaced rides beneath it.
    if let Some(public) = header.public {
        out.push(Element::label(ids::PROFILE_PUBLIC_NAME, public));
    }
    out.push(copy_id_button(st));

    if st.is_self() {
        out.push(Element::gesture_button(
            ids::PROFILE_EDIT_BUTTON,
            t::EDIT,
            true,
            Gesture::Profile(Action::OpenEdit),
        ));
    } else {
        out.push(Element::gesture_button(
            ids::PROFILE_FOLLOW_BUTTON,
            if st.followed { t::FOLLOWING } else { t::FOLLOW },
            true,
            Gesture::Profile(Action::Follow),
        ));
        out.push(Element::gesture_button(
            ids::PROFILE_START_DM_BUTTON,
            t::START_DM,
            true,
            Gesture::Profile(Action::StartDm),
        ));
        out.push(Element::gesture_button(
            ids::PROFILE_BLOCK_BUTTON,
            block_label(st),
            st.is_blocked.is_some(),
            Gesture::Profile(Action::ToggleBlock),
        ));
        // Report this account — beside block, OTHER view only (`moderation.md`
        // § User-initiated reporting → *App surface*); opens the shared report
        // sheet with the actor as its subject.
        if let Some(actor) = st.viewing.as_deref() {
            out.push(Element::gesture_button(
                ids::PROFILE_REPORT_BUTTON,
                t::REPORT,
                true,
                Gesture::Report(crate::report::Action::Open(Box::new(
                    crate::report::actor_target(actor),
                ))),
            ));
        }
        out.push(Element::gesture_button(
            ids::PROFILE_REQUEST_CONTACT_BUTTON,
            if st.knock_sent {
                t::REQUEST_CONTACT_SENT
            } else {
                t::REQUEST_CONTACT
            },
            !st.knock_sent,
            Gesture::Profile(Action::RequestContact),
        ));
        // The ward's ask — the contacts page's pair, on this page too
        // (`family-safety.md` § Child-initiated contact requests → *App
        // affordance*). Pending reads the durable `status.contact_requests`
        // first, so it is honest on an open that never saw the refusal; the
        // ask itself is offered only after the TYPED refusal, never on any
        // other failure (that would tell an unsupervised user they are
        // supervised).
        let peer = st.viewing.as_deref().unwrap_or_default();
        if app.family.contact_ask_pending(peer) || st.contact_ask_sent {
            out.push(Element::label(
                ids::CONTACT_REQUEST_PENDING,
                c::CONTACT_REQUEST_PENDING.to_string(),
            ));
        } else if st.guardian_refused {
            out.push(Element::gesture_button(
                ids::CONTACT_REQUEST_GUARDIAN_BUTTON,
                c::ASK_GUARDIAN,
                true,
                Gesture::Profile(Action::AskGuardian),
            ));
        }
        // Below the relationship actions, above the tab strip (profile.md
        // § The private section).
        private::elements(st, &mut out);
    }

    out.push(Element::gesture_button(
        ids::PROFILE_POSTS_TAB,
        t::POSTS,
        true,
        Gesture::Profile(Action::ShowPosts),
    ));
    out.push(Element::gesture_button(
        ids::PROFILE_TIERS_TAB,
        t::TIERS,
        true,
        Gesture::Profile(Action::ShowTiers),
    ));

    // The Posts tab's content (`profile.md` § The Posts tab).
    if st.tab == Tab::Posts {
        posts::elements(app, &mut out);
    }

    // SELF text-only edit form (`profile.md` § Where logic lives → publish/edit).
    if st.is_self()
        && let Some(edit) = &st.edit
    {
        out.push(Element::label(ids::PROFILE_EDIT_FORM, " "));
        out.push(
            Element::input(
                ids::PROFILE_EDIT_DISPLAY_NAME,
                edit.display_name.clone(),
                Field::Profile(ProfileField::DisplayName),
            )
            .labelled(t::EDIT_DISPLAY_NAME),
        );
        out.push(
            Element::input(
                ids::PROFILE_EDIT_BIO,
                edit.bio.clone(),
                Field::Profile(ProfileField::Bio),
            )
            .labelled(t::EDIT_BIO),
        );
        // The repeatable-links sub-list landmark (ui.yaml `profile-edit-form`'s
        // `profile-edit-link-list` child component).
        out.push(Element::label(ids::PROFILE_EDIT_LINK_LIST, " "));
        for (i, (label, url)) in edit.links.iter().enumerate() {
            out.push(
                Element::input(
                    ids::PROFILE_EDIT_LINK_LABEL,
                    label.clone(),
                    Field::Profile(ProfileField::LinkLabel(i)),
                )
                .labelled(t::EDIT_LINK_LABEL),
            );
            out.push(
                Element::input(
                    ids::PROFILE_EDIT_LINK_URL,
                    url.clone(),
                    Field::Profile(ProfileField::LinkUrl(i)),
                )
                .labelled(t::EDIT_LINK_URL),
            );
            out.push(Element::gesture_button(
                ids::PROFILE_EDIT_LINK_REMOVE_BUTTON,
                t::EDIT_REMOVE_LINK,
                true,
                Gesture::Profile(Action::RemoveLink(i)),
            ));
        }
        out.push(Element::gesture_button(
            ids::PROFILE_EDIT_LINK_ADD_BUTTON,
            t::EDIT_ADD_LINK,
            true,
            Gesture::Profile(Action::AddLink),
        ));
        // A path input, not an OS file picker (`tui.md` § Declared platform
        // absences 4) — `compose-file`'s shape. Staged only; the upload happens
        // at Save (`resolve_staged_image`).
        out.push(
            Element::input(
                ids::PROFILE_EDIT_AVATAR,
                edit.avatar_path.clone(),
                Field::Profile(ProfileField::Avatar),
            )
            .labelled(t::EDIT_AVATAR),
        );
        out.push(Element::gesture_button(
            ids::PROFILE_EDIT_AVATAR_REMOVE_BUTTON,
            t::EDIT_REMOVE_AVATAR,
            true,
            Gesture::Profile(Action::ClearAvatar),
        ));
        out.push(
            Element::input(
                ids::PROFILE_EDIT_BANNER,
                edit.banner_path.clone(),
                Field::Profile(ProfileField::Banner),
            )
            .labelled(t::EDIT_BANNER),
        );
        out.push(Element::gesture_button(
            ids::PROFILE_EDIT_BANNER_REMOVE_BUTTON,
            t::EDIT_REMOVE_BANNER,
            true,
            Gesture::Profile(Action::ClearBanner),
        ));
        out.push(Element::checkbox_gesture(
            ids::PROFILE_EDIT_AUDIENCE_TOGGLE,
            t::EDIT_AUDIENCE_TOGGLE,
            edit.audience_everyone(),
            Gesture::Profile(Action::ToggleAudience),
        ));
        out.push(Element::gesture_button(
            ids::PROFILE_EDIT_SAVE_BUTTON,
            t::EDIT_SAVE,
            true,
            Gesture::Profile(Action::SaveEdit),
        ));
        out.push(Element::gesture_button(
            ids::PROFILE_EDIT_CANCEL_BUTTON,
            t::EDIT_CANCEL,
            true,
            Gesture::Profile(Action::CancelEdit),
        ));
    }

    // OTHER Tiers-tab subscriber-browse offers (`profile.md` § Layout & flow →
    // Another's profile). The free "followers" tier is the header follow
    // button's job, so it is filtered out of the per-row list.
    if !st.is_self() && st.tab == Tab::Tiers {
        out.push(Element::label(ids::SUBSCRIPTION_OFFERS_SECTION, " "));
        for tier in st.offers.iter().filter(|t| t.name != FOLLOWERS_TIER) {
            out.push(Element::label(ids::SUBSCRIPTION_OFFER_ROW, " "));
            out.push(Element::label(
                ids::SUBSCRIPTION_OFFER_NAME,
                tier.name.clone(),
            ));
            // Always emit price + status per row (empty allowed) so the flat
            // per-id index stays aligned with the row index. The price and the
            // payment link below are the money plane's buyer half
            // (`dynamic-features.md` § Platform-family surface excision → *The
            // price-and-route class*): a store-safe build shows a priced tier
            // as an ordinary approval-gated one — name, description, Subscribe.
            #[cfg(feature = "payments")]
            out.push(Element::label(
                ids::SUBSCRIPTION_OFFER_PRICE,
                tier.price_hint.clone().unwrap_or_default(),
            ));
            if let Some(desc) = tier.description.as_deref().filter(|d| !d.is_empty()) {
                out.push(Element::label(ids::SUBSCRIPTION_OFFER_DESCRIPTION, desc));
            }
            out.push(Element::label(
                ids::SUBSCRIPTION_OFFER_STATUS,
                offer_status_display(st, &tier.name),
            ));
            // Only when the tier carries one — an offer with no external
            // checkout paints no link (ui.yaml: "opens external payment_url").
            #[cfg(feature = "payments")]
            if let Some(url) = tier.payment_url.as_deref().filter(|u| !u.is_empty()) {
                out.push(Element::gesture_button(
                    ids::SUBSCRIPTION_OFFER_PAYMENT_LINK,
                    s::PAYMENT_URL,
                    true,
                    Gesture::Profile(Action::OpenPaymentLink(url.to_string())),
                ));
            }
            out.push(Element::gesture_button(
                ids::SUBSCRIPTION_OFFER_SUBSCRIBE_BUTTON,
                s::SUBSCRIBE,
                true,
                Gesture::Profile(Action::Subscribe(tier.name.clone())),
            ));
        }
    }

    // SELF Tiers-tab author management §§1–5 (`tiers`).
    if st.is_self() && st.tab == Tab::Tiers {
        out.extend(tiers::elements(
            st,
            crate::settings::web::site_link(&app.settings)
                .origin
                .as_deref(),
        ));
    }

    out
}

/// ui.yaml's declared `profile.state_fields` (`profile.is_self` / `profile.actor_id`).
pub fn state_json(state: &ProfileState) -> serde_json::Value {
    serde_json::json!({
        "is_self": state.is_self(),
        "actor_id": state.actor_id(),
        // The actor-home slot the follow and knock read (`null` = this nest);
        // ui.yaml's `profile.home_nest_url`, the read-back of the nav patch's
        // field of the same name.
        "home_nest_url": state.actor_home,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::authed_app;

    fn self_app() -> App {
        let mut app = authed_app();
        app.page = Page::Profile;
        app.profile.self_actor_id = "ab".repeat(32);
        app.profile.self_address = Some("me@self.test".to_string());
        app
    }

    fn other_app() -> App {
        let mut app = self_app();
        app.profile.viewing = Some("cd".repeat(32));
        app
    }

    /// Give the profile state the nest handle every `Op` carries. Most render
    /// tests skip this (they inject an `Outcome` directly), but a test about
    /// which `Op` an action *emits* needs it — `apply_local` returns `None`
    /// without one. `NestClient::new` does no IO until `connect()`.
    fn with_nest(mut app: App) -> App {
        app.profile.nest = Some(fauna_client::NestClient::new(
            "http://127.0.0.1:1".to_string(),
            fauna_core::identity::ActorKeypair::generate(),
        ));
        app
    }

    /// Fold in the open-time block read as [`spawn_open_refresh`] would report
    /// it for the page's CURRENT open — i.e. a reply that is on time, so only a
    /// newer *press* can make it stale.
    fn hydrate_block(app: &mut App, blocked: bool) {
        let epoch = app.profile.open_epoch;
        apply_outcome(app, Outcome::BlockState { epoch, blocked });
    }

    fn ids(app: &App) -> Vec<String> {
        elements(app).into_iter().map(|e| e.id).collect()
    }

    fn text_of(app: &App, id: &str) -> String {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .map(|e| e.text)
            .unwrap_or_default()
    }

    fn count_id(app: &App, id: &str) -> usize {
        elements(app).iter().filter(|e| e.id == id).count()
    }

    fn a_tier(name: &str, price: Option<&str>) -> TierItem {
        TierItem {
            name: name.to_string(),
            rank: 1,
            description: None,
            price_hint: price.map(str::to_string),
            payment_url: None,
            asking_price: None,
            auto_approve: true,
            created_at: fauna_core::data::Timestamp(0),
            unlocks_post: None,
            hidden: false,
            extra: Default::default(),
        }
    }

    /// **The page read seeds the peer-anchor store** — the page-path arm of
    /// harvest rule 4 (`identity-succession.md` § the peer-profile harvest,
    /// "on the profile page"): an OTHER-profile open harvests the verified
    /// bytes it just fetched through the one-door gate, so a peer viewed on
    /// this page becomes a tier-1 anchor for the succession witness with no
    /// extra fetch. Driven through [`harvest_page_read`] (the factored arm the
    /// open's spawn rides) over a fake store and no session — the re-drive
    /// half is the roster sweep's own pattern and needs a live session.
    #[tokio::test]
    async fn an_other_profile_open_seeds_the_peer_anchor_store() {
        use fauna_conversations::backend::MemoryPeerAnchorStore;

        // The peer's signed profile, exactly as the page's header read fetched
        // it: a recovery head to seed and a home nest whose host becomes the
        // harvested dial domain.
        let peer = fauna_core::identity::ActorKeypair::from_secret([7u8; 32]);
        let head = fauna_core::recovery::ChainHead::new([0xC4; 32], 1);
        let profile = fauna_core::data::Profile {
            actor_id: peer.actor_id(),
            display_name: Some("Alice".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![fauna_core::data::NestEntry {
                nest_id: vec![],
                url: "https://home.example".into(),
                roles: vec![],
            }],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: fauna_core::data::InboxMode::Open,
            recovery_head: Some(head),
            audience: fauna_core::data::ProfileAudience::ThisNest,
            updated_at: fauna_core::data::Timestamp(0),
        };
        let bytes = fauna_core::encoding::sign_and_pack(&peer, &profile).expect("signs");

        let store = MemoryPeerAnchorStore::default();
        harvest_page_read(&peer.actor_id(), &bytes, &store, None).await;

        let stored = store.current();
        assert!(
            stored.known_chain_head(&peer.actor_id()).is_some(),
            "the page read must seed the peer's chain head"
        );
        assert_eq!(
            stored.known_anchor_domain(&peer.actor_id()).as_deref(),
            Some("home.example"),
            "the page read must seed the peer's dial domain"
        );
    }

    /// `profile-actor-id-copy-btn` reports the exact id it put on the clipboard
    /// as its `copied` attr — no e2e driver reads the OS clipboard, so this is
    /// what lets a test assert WHAT was copied. It names the VIEWED actor, and a
    /// fresh open starts with nothing copied.
    #[test]
    fn the_copy_button_reports_the_viewed_actors_id_and_a_fresh_open_clears_it() {
        let copied = |app: &App| {
            elements(app)
                .into_iter()
                .find(|e| e.id == "profile-actor-id-copy-btn")
                .and_then(|e| e.attrs.into_iter().find(|(k, _)| k == "copied"))
                .map(|(_, v)| v)
        };
        let mut app = other_app();
        assert_eq!(copied(&app), None, "nothing is copied before the press");

        apply_local(&mut app, Action::Copy);
        assert_eq!(
            copied(&app),
            Some("cd".repeat(32)),
            "OTHER copies the viewed id"
        );

        // What `App::open_profile(None)` does to the page state, minus its
        // network refresh.
        app.profile.viewing = None;
        app.profile.reset_for_open();
        assert_eq!(
            copied(&app),
            None,
            "a fresh open must not inherit the last copy"
        );

        apply_local(&mut app, Action::Copy);
        assert_eq!(
            copied(&app),
            Some("ab".repeat(32)),
            "SELF copies the viewer's own id"
        );
    }

    /// SELF paints the edit button + header (falling back to the cached handle),
    /// and NOT the OTHER-only relationship actions.
    #[test]
    fn self_profile_shows_edit_and_no_other_actions() {
        let app = self_app();
        let got = ids(&app);
        for required in [
            "profile-view",
            "page-heading",
            "profile-handle",
            "profile-actor-id-copy-btn",
            "profile-edit-button",
            "profile-posts-tab",
            "profile-tiers-tab",
        ] {
            assert!(got.contains(&required.to_string()), "must paint {required}");
        }
        for absent in [
            "profile-follow-button",
            "profile-start-dm-button",
            "profile-block-button",
        ] {
            assert!(
                !got.contains(&absent.to_string()),
                "SELF must not paint {absent}"
            );
        }
        // The header falls back to the cached handle before a published name.
        assert_eq!(text_of(&app, "profile-handle"), "me@self.test");
    }

    /// The SELF header follows a rename: the follower's push moves it to the
    /// new primary, and an unresolved push keeps what it had.
    #[test]
    fn the_self_header_follows_a_pushed_primary_rename() {
        let mut app = self_app();
        set_self_address(&mut app.profile, Some("me@renamed.test".to_string()));
        assert_eq!(text_of(&app, "profile-handle"), "me@renamed.test");
        set_self_address(&mut app.profile, None);
        assert_eq!(text_of(&app, "profile-handle"), "me@renamed.test");
    }

    /// With nothing resolved the header shows the actor id, never a bare handle.
    #[test]
    fn an_unresolved_self_header_shows_the_actor_id() {
        let mut app = self_app();
        app.profile.self_address = None;
        assert_eq!(text_of(&app, "profile-handle"), "ab".repeat(32));
    }

    /// A published display name upgrades the header over the fallback.
    #[test]
    fn a_published_display_name_upgrades_the_self_header() {
        let mut app = self_app();
        app.profile.header_name = Some("Ada Lovelace".to_string());
        assert_eq!(text_of(&app, "profile-handle"), "Ada Lovelace");
    }

    /// OTHER paints follow + the secondary actions (start-DM, block) and NOT the
    /// SELF edit button; the header falls straight to the actor id.
    #[test]
    fn other_profile_shows_follow_and_secondary_actions() {
        let app = other_app();
        let got = ids(&app);
        for required in [
            "profile-follow-button",
            "profile-start-dm-button",
            "profile-block-button",
            "profile-request-contact-button",
        ] {
            assert!(
                got.contains(&required.to_string()),
                "OTHER must paint {required}"
            );
        }
        assert!(
            !got.contains(&"profile-edit-button".to_string()),
            "OTHER must not paint the SELF edit button"
        );
        // The shared resolver's last fallback: the canonical short id
        // (value-formatting.md § Peer display label).
        assert_eq!(
            text_of(&app, "profile-handle"),
            format::short_id(&"cd".repeat(32))
        );
        assert!(
            !ids(&self_app()).contains(&"profile-request-contact-button".to_string()),
            "SELF has no one to knock",
        );
    }

    fn knock_ready(route: Option<&str>) -> App {
        let mut app = with_nest(other_app());
        app.profile.secret = Some(SecretArray32::from([7u8; 32]));
        app.profile.nest_url = "https://home.test".to_string();
        let epoch = app.profile.open_epoch;
        apply_outcome(
            &mut app,
            Outcome::HeaderName {
                epoch,
                name: None,
                actor_home: route.map(str::to_string),
                pictures: HeaderPictures::default(),
            },
        );
        app
    }

    fn follow_home(app: &mut App) -> Option<String> {
        match apply_local(app, Action::Follow) {
            Some(Op::Follow { home_nest_url, .. }) => home_nest_url,
            _ => panic!("the button must emit the follow"),
        }
    }

    /// The open target's carried home seeds the actor-home slot at once, so a
    /// follow pressed before the header read answers is already carried there;
    /// the header read's answer then replaces it (`profile.md` § Where logic
    /// lives → *The actor's home*). A hint naming the viewer's own nest is this
    /// nest, and a fresh open never inherits the last actor's home.
    #[test]
    fn the_carried_home_routes_the_follow_until_the_header_read_answers() {
        let mut app = with_nest(other_app());
        app.profile.secret = Some(SecretArray32::from([7u8; 32]));
        app.profile.nest_url = "https://home.test".to_string();
        // What `App::open_profile(Some(hex), home)` does to the page state,
        // minus its network refresh.
        app.profile.reset_for_open();
        app.profile
            .seed_actor_home(Some("https://met.test".to_string()));
        assert_eq!(follow_home(&mut app).as_deref(), Some("https://met.test"));
        assert_eq!(
            state_json(&app.profile)["home_nest_url"],
            "https://met.test",
            "the state read reports the slot"
        );

        let epoch = app.profile.open_epoch;
        apply_outcome(
            &mut app,
            Outcome::HeaderName {
                epoch,
                name: None,
                actor_home: Some("https://moved.test".to_string()),
                pictures: HeaderPictures::default(),
            },
        );
        assert_eq!(
            follow_home(&mut app).as_deref(),
            Some("https://moved.test"),
            "the header read's answer (the actor's own signed home) replaces the hint",
        );

        app.profile.reset_for_open();
        app.profile
            .seed_actor_home(Some("https://HOME.test/".to_string()));
        assert_eq!(
            follow_home(&mut app),
            None,
            "the viewer's own nest is local"
        );
        app.profile.reset_for_open();
        app.profile.seed_actor_home(None);
        assert_eq!(follow_home(&mut app), None);

        // SELF has no other actor's home.
        app.profile.viewing = None;
        app.profile.reset_for_open();
        app.profile
            .seed_actor_home(Some("https://met.test".to_string()));
        assert_eq!(app.profile.actor_home, None);
        assert_eq!(app.profile.carried_home, None);
    }

    fn knock_result(app: &mut App, peer: &str, result: crate::contacts::KnockSend) {
        apply_outcome(
            app,
            Outcome::Knock {
                peer: peer.to_string(),
                result,
            },
        );
    }

    fn an_ask(byte: u8) -> fauna_client_family::family::FamilyContactRequestInfo {
        fauna_client_family::family::FamilyContactRequestInfo {
            peer_actor_id: fauna_protocol::ByteBuf::from(vec![byte; 32]),
            peer_handle: "cd".into(),
            created_at: 0,
            extra: Default::default(),
        }
    }

    /// `profile-follow-button` follows the VIEWED actor at the home the
    /// open-time read found — the same home the knock takes — so a follow of an
    /// actor homed on another nest is carried there with the subscriber's own
    /// intent (`federated-follow.md` § Decision 2), and an actor on this nest
    /// names none.
    #[test]
    fn the_follow_names_the_viewed_actors_home() {
        for (route, want) in [
            (Some("https://peer.test"), Some("https://peer.test")),
            (None, None),
        ] {
            let mut app = knock_ready(route);
            match apply_local(&mut app, Action::Follow) {
                Some(Op::Follow {
                    author,
                    home_nest_url,
                    ..
                }) => {
                    assert_eq!(author, "cd".repeat(32));
                    assert_eq!(home_nest_url.as_deref(), want);
                }
                _ => panic!("the button must emit the follow"),
            }
        }
    }

    /// `profile-request-contact-button` knocks the VIEWED actor, on the route
    /// the open-time read took from that actor's own profile, signed from this
    /// nest — and once the nest accepts it the button reads "Request sent" and
    /// stops issuing knocks.
    #[test]
    fn the_knock_goes_to_the_viewed_actor_on_its_profiles_route() {
        let mut app = knock_ready(Some("https://peer.test"));
        let peer = "cd".repeat(32);
        match apply_local(&mut app, Action::RequestContact) {
            Some(Op::SendKnock {
                node_url,
                recipient,
                recipient_nest_url,
                ..
            }) => {
                assert_eq!(recipient, peer);
                assert_eq!(recipient_nest_url.as_deref(), Some("https://peer.test"));
                assert_eq!(node_url, "https://home.test");
            }
            _ => panic!("the button must emit the knock"),
        }
        assert_eq!(
            text_of(&app, "profile-request-contact-button"),
            t::REQUEST_CONTACT
        );

        knock_result(&mut app, &peer, crate::contacts::KnockSend::Sent);
        let button = elements(&app)
            .into_iter()
            .find(|e| e.id == "profile-request-contact-button")
            .expect("painted");
        assert_eq!(button.text, t::REQUEST_CONTACT_SENT);
        assert!(!button.enabled);
        assert!(
            apply_local(&mut app, Action::RequestContact).is_none(),
            "a sent knock is not re-sent",
        );
    }

    /// `family-safety.md` § Child-initiated contact requests, on this page:
    /// only the TYPED refusal reveals `contact-request-guardian-button` (a
    /// transport failure must not imply supervision), the refusal stays on
    /// `error-message`, and the landed ask swaps the button for
    /// `contact-request-pending` and lands in the durable family state.
    #[test]
    fn a_guardian_refused_knock_offers_the_ask_and_then_shows_it_pending() {
        let mut app = knock_ready(None);
        let peer = "cd".repeat(32);
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 0);
        assert_eq!(count_id(&app, "contact-request-pending"), 0);

        knock_result(
            &mut app,
            &peer,
            crate::contacts::KnockSend::Failed("inbox send: boom".into()),
        );
        assert_eq!(
            count_id(&app, "contact-request-guardian-button"),
            0,
            "a transport failure must not imply supervision",
        );

        knock_result(
            &mut app,
            &peer,
            crate::contacts::KnockSend::RefusedByGuardian,
        );
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 1);
        assert_eq!(
            app.errors.get(&Page::Profile).map(String::as_str),
            Some(c::GUARDIAN_APPROVAL_REQUIRED),
        );
        match apply_local(&mut app, Action::AskGuardian) {
            Some(Op::AskGuardian { peer: asked, .. }) => assert_eq!(asked, peer),
            _ => panic!("the ask button must emit the ask"),
        }

        apply_outcome(
            &mut app,
            Outcome::ContactRequested {
                peer: peer.clone(),
                requests: vec![an_ask(0xcd)],
            },
        );
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 0);
        assert_eq!(count_id(&app, "contact-request-pending"), 1);
        assert!(!app.errors.contains_key(&Page::Profile));
        assert!(app.family.contact_ask_pending(&peer));
    }

    /// The durable `status.contact_requests` paints pending on an open that
    /// never saw the refusal — a restart or a return visit.
    #[test]
    fn an_outstanding_ask_renders_pending_without_a_refusal() {
        let mut app = other_app();
        app.family.own_contact_requests = vec![an_ask(0xcd)];
        assert_eq!(count_id(&app, "contact-request-pending"), 1);
        app.profile.viewing = Some("ef".repeat(32));
        assert_eq!(
            count_id(&app, "contact-request-pending"),
            0,
            "another actor's ask is not this one's",
        );
    }

    /// A knock reply that outlived its open answers the actor you LEFT: it must
    /// neither flip this profile's button to "Sent" nor offer the ask about
    /// the wrong person.
    #[test]
    fn a_knock_reply_for_a_previous_open_does_not_paint_on_this_one() {
        let mut app = knock_ready(None);
        let left = "cd".repeat(32);
        app.profile.reset_for_open();
        app.profile.viewing = Some("ef".repeat(32));

        knock_result(
            &mut app,
            &left,
            crate::contacts::KnockSend::RefusedByGuardian,
        );
        knock_result(&mut app, &left, crate::contacts::KnockSend::Sent);
        assert_eq!(count_id(&app, "contact-request-guardian-button"), 0);
        assert_eq!(
            text_of(&app, "profile-request-contact-button"),
            t::REQUEST_CONTACT
        );

        apply_outcome(
            &mut app,
            Outcome::ContactRequested {
                peer: left,
                requests: vec![],
            },
        );
        assert_eq!(count_id(&app, "contact-request-pending"), 0);
    }

    /// The edit form registers its fields only when open, and the field buffers
    /// round-trip through `field`/`set_field`.
    #[test]
    fn the_edit_form_registers_and_its_fields_round_trip() {
        let mut app = self_app();
        assert_eq!(count_id(&app, "profile-edit-form"), 0, "closed by default");

        app.profile.edit = Some(EditForm::default());
        for required in [
            "profile-edit-form",
            "profile-edit-display-name",
            "profile-edit-bio",
            "profile-edit-link-add-button",
            "profile-edit-save-button",
            "profile-edit-cancel-button",
        ] {
            assert_eq!(count_id(&app, required), 1, "open form paints {required}");
        }

        set_field(
            &mut app.profile,
            ProfileField::DisplayName,
            "Ada".to_string(),
        );
        set_field(&mut app.profile, ProfileField::Bio, "hi".to_string());
        assert_eq!(field(&app.profile, &ProfileField::DisplayName), "Ada");
        assert_eq!(text_of(&app, "profile-edit-display-name"), "Ada");
        assert_eq!(text_of(&app, "profile-edit-bio"), "hi");
    }

    /// The open form paints the avatar/banner staged-path inputs and their
    /// remove buttons, closed like every other `profile-edit-*` field.
    #[test]
    fn the_edit_form_registers_avatar_and_banner_fields() {
        let mut app = self_app();
        for id in [
            "profile-edit-avatar",
            "profile-edit-avatar-remove-button",
            "profile-edit-banner",
            "profile-edit-banner-remove-button",
        ] {
            assert_eq!(count_id(&app, id), 0, "closed by default");
        }

        app.profile.edit = Some(EditForm::default());
        for id in [
            "profile-edit-avatar",
            "profile-edit-avatar-remove-button",
            "profile-edit-banner",
            "profile-edit-banner-remove-button",
        ] {
            assert_eq!(count_id(&app, id), 1, "open form paints {id}");
        }
        // Empty by default — a real OS picker has nothing pre-filled either.
        assert_eq!(text_of(&app, "profile-edit-avatar"), "");
        assert_eq!(text_of(&app, "profile-edit-banner"), "");

        set_field(
            &mut app.profile,
            ProfileField::Avatar,
            "/tmp/pic.jpg".to_string(),
        );
        assert_eq!(field(&app.profile, &ProfileField::Avatar), "/tmp/pic.jpg");
        assert_eq!(text_of(&app, "profile-edit-avatar"), "/tmp/pic.jpg");
    }

    /// Tapping remove stages a `Clear`; typing a path afterward overrides it
    /// back to a pending upload (picking wins over removing — the last action
    /// a human took should win, same rule linux's form follows).
    #[test]
    fn clear_button_stages_removal_and_a_later_typed_path_overrides_it() {
        let mut app = self_app();
        app.profile.edit = Some(EditForm::default());

        apply_local(&mut app, Action::ClearAvatar);
        let edit = app.profile.edit.as_ref().unwrap();
        assert!(edit.avatar_clear, "remove tap stages a clear");
        assert_eq!(edit.avatar_path, "", "remove tap empties any typed path");
        assert!(matches!(
            staged_image(&edit.avatar_path, edit.avatar_clear),
            StagedImage::Clear
        ));

        set_field(
            &mut app.profile,
            ProfileField::Avatar,
            "/tmp/new.png".to_string(),
        );
        let edit = app.profile.edit.as_ref().unwrap();
        assert!(matches!(
            staged_image(&edit.avatar_path, edit.avatar_clear),
            StagedImage::Upload(p) if p == "/tmp/new.png"
        ));
    }

    /// The pure resolve-priority rule `Op::Save` relies on: an empty path with
    /// no clear tap is `Keep` (text-only save, the v1 behaviour); a typed path
    /// always wins over a pending clear.
    #[test]
    fn staged_image_priority_is_path_then_clear_then_keep() {
        assert!(matches!(staged_image("", false), StagedImage::Keep));
        assert!(matches!(staged_image("  ", false), StagedImage::Keep));
        assert!(matches!(staged_image("", true), StagedImage::Clear));
        assert!(
            matches!(staged_image("/a/b.png", true), StagedImage::Upload(p) if p == "/a/b.png")
        );
    }

    // ── The remote read (`profile.md` § Where logic lives → The remote read) ──

    fn remote_profile_bytes(peer: &ActorKeypair, home: &str) -> Vec<u8> {
        let profile = fauna_core::data::Profile {
            actor_id: peer.actor_id(),
            display_name: Some("Remote Ada".into()),
            bio: None,
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![fauna_core::data::NestEntry {
                nest_id: vec![],
                url: home.into(),
                roles: vec![],
            }],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: fauna_core::data::InboxMode::Open,
            recovery_head: None,
            audience: fauna_core::data::ProfileAudience::Everyone,
            updated_at: fauna_core::data::Timestamp(0),
        };
        fauna_core::encoding::sign_and_pack(peer, &profile).expect("signs")
    }

    fn profile_not_found() -> fauna_protocol::RpcError {
        fauna_protocol::RpcError::new(
            fauna_protocol::RpcError::CODE_PROFILE_NOT_FOUND,
            "error.test",
        )
    }

    /// An OTHER header whose actor this nest does not hold falls through to the
    /// relay at the carried home, and the relayed bytes feed the header's name
    /// and the actor-home rule exactly as a local read's do — the signed home
    /// (`nests[0]`) replacing the carried hint.
    #[tokio::test]
    async fn the_other_header_relays_to_the_carried_home_on_a_local_not_found() {
        let peer = ActorKeypair::from_secret([9u8; 32]);
        let actor = peer.actor_id().to_hex();
        let bytes = remote_profile_bytes(&peer, "https://signed-home.test");
        let nest = Arc::new(
            fauna_client_testkit::RejectingRequester::new()
                .reject("fauna.profile.get", profile_not_found())
                .reply(
                    "fauna.profile.get_remote",
                    &fauna_protocol::profile::ProfileGetReply {
                        body: fauna_protocol::ByteBuf::from(bytes.clone()),
                        // The relay's stamp: the home's identity the reader's
                        // own nest dial-proved.
                        home_nest_actor_id: Some("ab".repeat(32)),
                        ..Default::default()
                    },
                ),
        );

        let HeaderFetch {
            name,
            bytes: got,
            pictures,
        } = fetch_header_profile(Arc::clone(&nest), &actor, Some("https://met.test")).await;

        assert_eq!(name.as_deref(), Some("Remote Ada"));
        assert_eq!(
            pictures.home,
            Some(fauna_client::ForeignHome::new(
                "https://met.test",
                Some(&"ab".repeat(32))
            )),
            "a relayed profile's pictures rest on the home it was relayed from, \
             pinned against the identity the relay stamped"
        );
        assert_eq!(got.as_deref(), Some(bytes.as_slice()));
        assert_eq!(
            nest.kinds(),
            vec!["fauna.profile.get", "fauna.profile.get_remote"]
        );
        let (_, relay) = &nest.payloads()[1];
        assert_eq!(
            relay["nest_url"], "https://met.test",
            "asked of the carried home"
        );
        assert_eq!(
            fauna_client_profile::actor_home(
                Some("https://met.test"),
                &peer.actor_id(),
                got.as_deref(),
                "https://own.test",
            )
            .as_deref(),
            Some("https://signed-home.test"),
            "the relayed profile's signed home replaces the carried hint"
        );
    }

    /// A home that does not serve the profile (not opted in, not hosted,
    /// unreachable), or no home at all, is the actor-id fallback: no name, no
    /// bytes — and nothing for `error-message`, since the read reports none.
    #[tokio::test]
    async fn the_other_header_falls_back_to_the_id_when_the_relay_does_not_serve() {
        let actor = "cd".repeat(32);
        let refusing = Arc::new(
            fauna_client_testkit::RejectingRequester::new()
                .reject("fauna.profile.get", profile_not_found())
                .reject("fauna.profile.get_remote", profile_not_found()),
        );
        assert_eq!(
            fetch_header_profile(Arc::clone(&refusing), &actor, Some("https://met.test")).await,
            HeaderFetch::default()
        );

        // Unreachable home: the relay's own failure is the same fallback.
        let unreachable = Arc::new(
            fauna_client_testkit::RejectingRequester::new()
                .reject("fauna.profile.get", profile_not_found()),
        );
        assert_eq!(
            fetch_header_profile(Arc::clone(&unreachable), &actor, Some("https://met.test")).await,
            HeaderFetch::default()
        );

        // No home in hand: the local not_found stands, the relay is never asked.
        let homeless = Arc::new(
            fauna_client_testkit::RejectingRequester::new()
                .reject("fauna.profile.get", profile_not_found()),
        );
        assert_eq!(
            fetch_header_profile(Arc::clone(&homeless), &actor, None).await,
            HeaderFetch::default()
        );
        assert_eq!(homeless.kinds(), vec!["fauna.profile.get"]);
    }

    // ── The header's pictures (`profile-avatar`, `profile-banner`) ──

    fn png_and_hash() -> (Vec<u8>, String) {
        let png = fauna_media::test_fixtures::build_png(4, 4);
        let hash = hex::encode(fauna_core::encoding::content_hash(&png).digest());
        (png, hash)
    }

    /// A picture element is present only when the profile names the picture,
    /// paints empty until its art lands (loading and a failed fetch look the
    /// same — never `error-message`), and an answer from a superseded open
    /// is dropped.
    #[test]
    fn the_header_paints_only_the_pictures_the_profile_names() {
        let mut app = other_app();
        let epoch = app.profile.open_epoch;
        apply_outcome(
            &mut app,
            Outcome::HeaderName {
                epoch,
                name: None,
                actor_home: None,
                pictures: HeaderPictures::default(),
            },
        );
        assert_eq!(count_id(&app, "profile-avatar"), 0, "no avatar, no element");
        assert_eq!(count_id(&app, "profile-banner"), 0);

        let (png, hash) = png_and_hash();
        apply_outcome(
            &mut app,
            Outcome::HeaderName {
                epoch,
                name: None,
                actor_home: None,
                pictures: HeaderPictures {
                    avatar: Some(hash),
                    banner: None,
                    home: None,
                },
            },
        );
        assert_eq!(count_id(&app, "profile-avatar"), 1);
        assert_eq!(count_id(&app, "profile-banner"), 0);
        assert!(!text_of(&app, "profile-avatar").contains(crate::thumbnail::HALF_BLOCK));

        let art = || HeaderArt {
            avatar: crate::thumbnail::rasterize(&png, crate::thumbnail::THUMBNAIL_COLS),
            banner: None,
        };
        apply_outcome(
            &mut app,
            Outcome::Pictures {
                epoch: epoch.wrapping_sub(1),
                art: Box::new(art()),
            },
        );
        assert!(
            !text_of(&app, "profile-avatar").contains(crate::thumbnail::HALF_BLOCK),
            "a superseded open's art paints nobody"
        );
        apply_outcome(
            &mut app,
            Outcome::Pictures {
                epoch,
                art: Box::new(art()),
            },
        );
        assert!(text_of(&app, "profile-avatar").contains(crate::thumbnail::HALF_BLOCK));
        assert!(app.errors.get(&Page::Profile).is_none());
    }

    /// The host pick (`profile.md` § Where logic lives → *The remote read*,
    /// *Bytes direct, as for a relayed post*): a profile this nest served reads
    /// its pictures from this nest; a relayed one from its home — asking this
    /// nest nothing — and bytes that are not the named content paint nothing.
    #[tokio::test]
    async fn the_pictures_come_from_the_nest_that_served_the_profile() {
        let (png, hash) = png_and_hash();
        let path = fauna_nest_http::paths::blob::by_hash(&hash);
        let own = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        let foreign = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        let asked = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (plane, log) = (Arc::clone(&foreign), Arc::clone(&asked));
        let blobs = fauna_client::PostBlobHosts::with_foreign(
            Arc::clone(&own) as Arc<dyn NestContentApi>,
            Arc::new(move |home: &fauna_client::ForeignHome| {
                log.lock().unwrap().push(home.clone());
                Arc::clone(&plane) as Arc<dyn NestContentApi>
            }),
        );
        let pictures = |home: Option<fauna_client::ForeignHome>| HeaderPictures {
            avatar: Some(hash.clone()),
            banner: Some(hash.clone()),
            home,
        };

        own.set_ok(fauna_nest_http::Verb::Get, &path, png.clone());
        let art = fetch_pictures(&blobs, &pictures(None)).await;
        assert!(art.avatar.is_some() && art.banner.is_some());
        assert!(asked.lock().unwrap().is_empty(), "this nest's own profile");

        let home = fauna_client::ForeignHome::new("https://home.example", None);
        let own = Arc::new(fauna_nest_http::FakeNestContentApi::new());
        let blobs = fauna_client::PostBlobHosts::with_foreign(
            Arc::clone(&own) as Arc<dyn NestContentApi>,
            Arc::new({
                let (plane, log) = (Arc::clone(&foreign), Arc::clone(&asked));
                move |home: &fauna_client::ForeignHome| {
                    log.lock().unwrap().push(home.clone());
                    Arc::clone(&plane) as Arc<dyn NestContentApi>
                }
            }),
        );
        foreign.set_ok(fauna_nest_http::Verb::Get, &path, png);
        let art = fetch_pictures(&blobs, &pictures(Some(home.clone()))).await;
        assert!(art.avatar.is_some(), "the home's bytes paint");
        assert!(asked.lock().unwrap().contains(&home));
        assert!(own.calls().is_empty(), "this nest holds none of its bytes");

        foreign.set_ok(
            fauna_nest_http::Verb::Get,
            &path,
            b"not the named content".to_vec(),
        );
        let art = fetch_pictures(&blobs, &pictures(Some(home))).await;
        assert!(art.avatar.is_none() && art.banner.is_none());
    }

    /// `profile-edit-audience-toggle` paints on the open form from the base's
    /// audience, flips on a press, and only a moved toggle reaches the save —
    /// flipped back to where it started, it is untouched again.
    #[test]
    fn the_audience_toggle_starts_from_the_base_and_stages_only_a_change() {
        let checked = |app: &App| {
            elements(app)
                .into_iter()
                .find(|e| e.id == "profile-edit-audience-toggle")
                .map(|e| match e.role {
                    crate::element::Role::Checkbox { checked, .. } => checked,
                    _ => panic!("the toggle is a checkbox"),
                })
        };
        let mut app = with_nest(self_app());
        app.profile.secret = Some(SecretArray32::new([3u8; 32]));
        assert_eq!(checked(&app), None, "closed form paints no toggle");

        app.profile.edit = Some(EditForm::default());
        apply_outcome(
            &mut app,
            Outcome::EditBase {
                base_body: None,
                last_direct_body: None,
                display_name: None,
                bio: None,
                links: vec![],
                audience_everyone: true,
            },
        );
        assert_eq!(checked(&app), Some(true), "opens on the stored audience");

        let saved_audience = |app: &mut App| match apply_local(app, Action::SaveEdit) {
            Some(Op::Save { audience, .. }) => audience,
            _ => panic!("Save emits Op::Save"),
        };
        assert_eq!(saved_audience(&mut app), None, "untouched keeps the base");

        apply_local(&mut app, Action::ToggleAudience);
        assert_eq!(checked(&app), Some(false));
        assert_eq!(
            saved_audience(&mut app),
            Some(false),
            "a moved toggle saves"
        );

        apply_local(&mut app, Action::ToggleAudience);
        assert_eq!(checked(&app), Some(true));
        assert_eq!(
            saved_audience(&mut app),
            None,
            "back at the base is untouched"
        );

        // A press made before the base lands survives its arrival.
        app.profile.edit = Some(EditForm::default());
        apply_local(&mut app, Action::ToggleAudience);
        apply_outcome(
            &mut app,
            Outcome::EditBase {
                base_body: None,
                last_direct_body: None,
                display_name: None,
                bio: None,
                links: vec![],
                audience_everyone: false,
            },
        );
        assert_eq!(checked(&app), Some(true), "the early press stands");
        assert_eq!(saved_audience(&mut app), Some(true));
    }

    /// The owner's last identity-signed word on tui's edit form (`profile.md`
    /// § Where logic lives → *Profile publish/edit* → *Edit surface*): over an
    /// external app's edit, the base load opens the toggle on the audience the
    /// owner's own row signed and hands that row to the save, so an untouched
    /// save signs it again; with the row withheld the toggle opens off — what
    /// that save signs.
    #[tokio::test]
    async fn the_owners_own_word_opens_the_toggle_and_rides_the_save() {
        use fauna_core::data::{Capability, DeviceAuthorization, Profile, ProfileAudience};
        use fauna_core::encoding::{EmbedAsBytes, canonical_encode, sign_and_pack, sign_envelope};

        let secret = SecretArray32::new([3u8; 32]);
        let owner = ActorKeypair::from_secret(secret.to_array());
        let profile = |bio: &str, audience: ProfileAudience, at: u64| Profile {
            actor_id: owner.actor_id(),
            display_name: Some("Ada".into()),
            bio: Some(bio.into()),
            avatar: None,
            banner: None,
            links: vec![],
            nests: vec![],
            admin_nests: vec![],
            load_hint: None,
            inbox_mode: fauna_core::data::InboxMode::Open,
            recovery_head: None,
            audience,
            updated_at: fauna_core::data::Timestamp(at),
        };
        let direct = sign_and_pack(&owner, &profile("mine", ProfileAudience::Everyone, 10))
            .expect("the owner's own publish");
        // The external edit, signed by the nest-held sub-key.
        let sub = ActorKeypair::from_secret([4u8; 32]);
        let cert = DeviceAuthorization {
            actor_id: owner.actor_id(),
            device_key: sub.actor_id().0,
            capabilities: vec![Capability::UpdateProfile],
            created_at: fauna_core::data::Timestamp(0),
            expires_at: None,
        };
        let (cert_bytes, cert_env) = sign_envelope(&owner, &cert).expect("the cert signs");
        let (bytes, env) = sign_envelope(
            &sub,
            &profile("edited elsewhere", ProfileAudience::Everyone, 20),
        )
        .expect("the sub-key signs");
        let delegated = canonical_encode(
            &EmbedAsBytes::from_signed(bytes, env)
                .with_signer_auth(EmbedAsBytes::from_signed(cert_bytes, cert_env)),
        )
        .expect("the delegated wire encodes");
        let accounts = crate::session::registry_over(Arc::new(
            fauna_client_accounts::InMemorySecretStore::new(),
        ));

        for (served, opens) in [(Some(direct.clone()), true), (None, false)] {
            let nest = Arc::new(fauna_client_testkit::RejectingRequester::new().reply(
                "fauna.profile.get",
                &fauna_protocol::profile::ProfileGetReply {
                    body: fauna_protocol::ByteBuf::from(delegated.clone()),
                    last_direct_body: served.clone().map(fauna_protocol::ByteBuf::from),
                    ..Default::default()
                },
            ));
            let outcome = load_edit_base(nest, &secret, &accounts).await;
            let Outcome::EditBase {
                audience_everyone,
                last_direct_body,
                bio,
                ..
            } = &outcome
            else {
                panic!("the base load answers EditBase");
            };
            assert_eq!(
                *audience_everyone, opens,
                "the toggle opens on what the save will sign"
            );
            assert_eq!(last_direct_body, &served);
            assert_eq!(bio.as_deref(), Some("edited elsewhere"));

            let mut app = with_nest(self_app());
            app.profile.secret = Some(secret.clone());
            app.profile.edit = Some(EditForm::default());
            apply_outcome(&mut app, outcome);
            match apply_local(&mut app, Action::SaveEdit) {
                Some(Op::Save {
                    audience,
                    last_direct_body,
                    ..
                }) => {
                    assert_eq!(audience, None, "an untouched toggle stages nothing");
                    assert_eq!(last_direct_body, served, "the row rides the save");
                }
                _ => panic!("Save emits Op::Save"),
            }
        }
    }

    /// `Keep`/`Clear` resolve with no HTTP plane at all — a text-only save (or
    /// a remove) must not require the content API to be wired.
    #[tokio::test]
    async fn resolve_staged_image_keep_and_clear_need_no_content_plane() {
        assert!(matches!(
            resolve_staged_image(StagedImage::Keep, None).await,
            Ok(ProfileImageEdit::Keep)
        ));
        assert!(matches!(
            resolve_staged_image(StagedImage::Clear, None).await,
            Ok(ProfileImageEdit::Clear)
        ));
    }

    /// An `Upload` with no content plane fails loudly rather than silently
    /// downgrading to `Keep` — the same "never drop a picked file" rule feed's
    /// `submit_post` follows for `compose-file`.
    #[tokio::test]
    async fn resolve_staged_image_upload_without_content_plane_fails() {
        let err = resolve_staged_image(StagedImage::Upload("/tmp/x.jpg".to_string()), None)
            .await
            .unwrap_err();
        assert!(err.contains("pre-auth"), "got {err:?}");
    }

    /// An `Upload` reads the real file, uploads it through the shared
    /// public-post blob path, and resolves the nest's returned hash into a
    /// `ProfileImageEdit::Set` — the same upload mechanism feed's
    /// `compose-file` rides (`media.md` § Encryption at rest).
    #[tokio::test]
    async fn resolve_staged_image_upload_resolves_the_uploaded_hash() {
        let dir = std::env::temp_dir().join(format!(
            "fauna-tui-avatar-test-{}-{}",
            std::process::id(),
            line!()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("avatar.png");
        let bytes = b"not really a png, just test bytes";
        std::fs::write(&path, bytes).unwrap();

        // The shared upload refuses an echo that differs from the local
        // blake3 (`security.md` § sign-over-cid), so the fake nest echoes it.
        let hash_hex: String = fauna_core::encoding::content_hash(bytes)
            .digest()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let fake = fauna_nest_http::FakeNestContentApi::new();
        fake.set_ok(
            fauna_nest_http::Verb::PostMultipartBlob,
            fauna_nest_http::paths::blob::UPLOAD,
            format!(r#"{{"hash": "{hash_hex}"}}"#),
        );

        let edit = resolve_staged_image(
            StagedImage::Upload(path.to_string_lossy().to_string()),
            Some(&fake),
        )
        .await
        .unwrap();
        assert_eq!(edit, ProfileImageEdit::set_from_hex(&hash_hex).unwrap());

        std::fs::remove_dir_all(&dir).unwrap();
    }

    /// Adding then removing a link row grows/shrinks the indexed inputs.
    #[test]
    fn link_rows_add_and_remove() {
        let mut app = self_app();
        app.profile.edit = Some(EditForm::default());
        apply_local(&mut app, Action::AddLink);
        apply_local(&mut app, Action::AddLink);
        assert_eq!(count_id(&app, "profile-edit-link-label"), 2);
        set_field(
            &mut app.profile,
            ProfileField::LinkUrl(1),
            "https://x".to_string(),
        );
        assert_eq!(field(&app.profile, &ProfileField::LinkUrl(1)), "https://x");
        apply_local(&mut app, Action::RemoveLink(0));
        assert_eq!(count_id(&app, "profile-edit-link-label"), 1);
    }

    /// The **Tiers-tab re-read door** (`monetization.md` § Pillar 1 → *The
    /// Tiers-tab re-read door*): EVERY activation of `profile-tiers-tab`
    /// re-reads, including one that does not change the tab.
    ///
    /// tui is the reference implementation of this ruling — the shape the other
    /// six apps were lifted onto on 2026-08-12 — but nothing pinned it, so a
    /// refactor that made the load conditional on the tab actually changing
    /// (the obvious "don't reload what's already shown" optimization) would
    /// have silently retired the door on the app that defines it. That is not
    /// hypothetical: it is exactly the state linux, web, windows and apple were
    /// found in. The re-activation case is the load-bearing one, because
    /// re-clicking the tab is how a subscriber asks "did the author approve me
    /// yet?" — and there is no push kind for a subscribe grant to answer it.
    #[test]
    fn every_tiers_tab_activation_reloads_the_offers_even_when_already_there() {
        let mut app = with_nest(other_app());

        let first = apply_local(&mut app, Action::ShowTiers);
        assert!(
            matches!(first, Some(Op::LoadOffers { .. })),
            "the first activation must load the offers"
        );
        assert!(matches!(app.profile.tab, Tab::Tiers));

        // Already on the tab — the activation changes no state at all, so a
        // change-gated implementation would emit nothing here.
        let again = apply_local(&mut app, Action::ShowTiers);
        assert!(
            matches!(again, Some(Op::LoadOffers { .. })),
            "re-activating the tab while already on it must STILL load the \
             offers — the ruled door"
        );

        // The SELF half of the same ruling: an author re-opening Tiers is
        // asking whether a new request landed.
        let mut me = with_nest(self_app());
        assert!(matches!(
            apply_local(&mut me, Action::ShowTiers),
            Some(Op::LoadAuthor { .. })
        ));
        let me_again = apply_local(&mut me, Action::ShowTiers);
        assert!(
            matches!(me_again, Some(Op::LoadAuthor { .. })),
            "re-activating the SELF Tiers tab must re-read §§1–5"
        );
    }

    /// The Tiers tab renders one offer row per non-"followers" tier, with the
    /// shared status badge; the free tier is filtered out (it is the follow
    /// button's job).
    #[test]
    fn other_tiers_tab_renders_offer_rows_and_status() {
        let mut app = other_app();
        apply_local(&mut app, Action::ShowTiers); // sets tab = Tiers
        apply_outcome(
            &mut app,
            Outcome::Offers {
                offers: vec![a_tier("followers", None), a_tier("gold", Some("$5/mo"))],
                status_tier: None,
            },
        );
        assert!(
            elements(&app)
                .iter()
                .any(|e| e.id == "subscription-offers-section")
        );
        assert_eq!(
            count_id(&app, "subscription-offer-row"),
            1,
            "followers is filtered out"
        );
        assert_eq!(text_of(&app, "subscription-offer-name"), "gold");
        #[cfg(feature = "payments")]
        assert_eq!(text_of(&app, "subscription-offer-price"), "$5/mo");
        // No held tier, no pending ⇒ "Not subscribed".
        assert_eq!(text_of(&app, "subscription-offer-status"), "Not subscribed");

        // A confirmed held tier flips the badge to "Subscribed".
        apply_outcome(
            &mut app,
            Outcome::Subscribed {
                offers: vec![a_tier("gold", Some("$5/mo"))],
                status_tier: Some("gold".to_string()),
                tier: "gold".to_string(),
                queued: false,
            },
        );
        assert_eq!(text_of(&app, "subscription-offer-status"), "Subscribed");
    }

    /// The block toggle label reflects `is_blocked` (tui is in the flipped set).
    #[test]
    fn block_toggle_label_flips_on_state() {
        let mut app = other_app();
        assert_eq!(text_of(&app, "profile-block-button"), "Block");
        apply_outcome(&mut app, Outcome::BlockToggled(true));
        assert_eq!(text_of(&app, "profile-block-button"), "Unblock");
        apply_outcome(&mut app, Outcome::BlockToggled(false));
        assert_eq!(text_of(&app, "profile-block-button"), "Block");
    }

    /// A LATE open-time hydration read must not clobber the block edge the user
    /// has authored since it was issued.
    ///
    /// [`spawn_open_refresh`] fires `contacts.list` at open and reports
    /// [`Outcome::BlockState`] whenever it returns — which, over a real WS
    /// round-trip, is routinely **after** the viewer has already pressed the
    /// toggle. Its answer describes the edge as it was *before* that press, so
    /// applying it unconditionally rewinds `is_blocked` to the pre-press value.
    #[test]
    fn a_late_open_hydration_does_not_rewind_the_user_toggle() {
        let mut app = other_app();
        assert_eq!(text_of(&app, "profile-block-button"), "Block");

        // The viewer blocks: the toggle lands `blocked`.
        apply_outcome(&mut app, Outcome::BlockToggled(true));
        assert_eq!(text_of(&app, "profile-block-button"), "Unblock");

        // The open-time read — issued BEFORE that press, so it still says
        // not-blocked — finally returns. Same open, so the epoch matches; it is
        // the press, not the nav, that makes it stale.
        hydrate_block(&mut app, false);
        assert_eq!(
            text_of(&app, "profile-block-button"),
            "Unblock",
            "a hydration read issued before the press must not rewind the edge \
             the press authored"
        );
    }

    /// The consequence the e2e red actually reads: with `is_blocked` rewound by
    /// a late hydration, the viewer's SECOND press re-blocks instead of
    /// unblocking — the toggle never flips back, and nothing errors, because
    /// the wrong call succeeded.
    #[test]
    fn the_press_after_a_late_hydration_still_unblocks() {
        let mut app = with_nest(other_app());

        apply_outcome(&mut app, Outcome::BlockToggled(true));
        hydrate_block(&mut app, false);

        assert!(
            matches!(
                apply_local(&mut app, Action::ToggleBlock),
                Some(Op::ToggleBlock {
                    currently_blocked: true,
                    ..
                })
            ),
            "the second press must issue knocks_UNBLOCK against the blocked \
             edge; issuing knocks_block again succeeds on the wire and leaves \
             the label reading 'Unblock' with no error — the e2e signature"
        );
    }

    /// Before the open-time read lands, the toggle does not know which edge it
    /// is on, so it must not act. A re-open of an actor the viewer already
    /// blocks starts from not-known, and a press that ran on a not-blocked
    /// default would issue `knocks_block` again: it succeeds on the wire, the
    /// label stays "Unblock", and nothing errors. On a box where the read takes
    /// a slow round trip, that is the e2e signature `the unblock lands: not
    /// reached`.
    #[test]
    fn the_toggle_is_inert_until_the_open_time_read_lands() {
        let mut app = with_nest(other_app());
        app.profile.reset_for_open();
        app.profile.viewing = Some("cd".repeat(32));

        let toggle = elements(&app)
            .into_iter()
            .find(|e| e.id == "profile-block-button")
            .expect("the toggle paints while the read is in flight");
        assert!(
            !toggle.enabled,
            "the toggle is disabled until the edge is known"
        );
        assert!(
            apply_local(&mut app, Action::ToggleBlock).is_none(),
            "a press before the read lands issues nothing"
        );

        hydrate_block(&mut app, true);
        assert!(
            matches!(
                apply_local(&mut app, Action::ToggleBlock),
                Some(Op::ToggleBlock {
                    currently_blocked: true,
                    ..
                })
            ),
            "once the read says blocked, the press unblocks"
        );
    }

    /// The press guard is scoped to ONE open: re-opening the page must hydrate
    /// from the nest again, or the first block of a session would pin the
    /// toggle's state for every profile opened afterwards.
    #[test]
    fn re_opening_the_page_hydrates_from_the_nest_again() {
        let mut app = other_app();
        apply_outcome(&mut app, Outcome::BlockToggled(true));
        hydrate_block(&mut app, false);
        assert_eq!(text_of(&app, "profile-block-button"), "Unblock");

        // A fresh open — of this actor or another. The next read is the
        // authority again.
        app.profile.reset_for_open();
        hydrate_block(&mut app, true);
        assert_eq!(
            text_of(&app, "profile-block-button"),
            "Unblock",
            "after a re-open the open-time read is the authority again"
        );
        hydrate_block(&mut app, false);
        assert_eq!(text_of(&app, "profile-block-button"), "Block");
    }

    /// The other half of the same class: a read issued for the profile you were
    /// looking at a moment ago must not land on the one you are looking at now.
    ///
    /// [`spawn_open_refresh`] captures the actor at spawn time, so its answer is
    /// correct *about that actor* — which is exactly what makes it dangerous
    /// once the viewer has navigated on. Both open-time reads are covered: the
    /// header would show the previous actor's display name.
    #[test]
    fn an_open_time_read_never_lands_on_a_later_open() {
        let mut app = other_app();
        let stale = app.profile.open_epoch;

        // The viewer navigates to a different actor while both reads are in
        // flight for the first one.
        app.profile.reset_for_open();
        app.profile.viewing = Some("ef".repeat(32));

        apply_outcome(
            &mut app,
            Outcome::HeaderName {
                epoch: stale,
                name: Some("The previous actor".to_string()),
                actor_home: Some("https://stale.example".to_string()),
                pictures: HeaderPictures::default(),
            },
        );
        apply_outcome(
            &mut app,
            Outcome::BlockState {
                epoch: stale,
                blocked: true,
            },
        );

        assert_eq!(
            text_of(&app, "profile-handle"),
            format::short_id(&"ef".repeat(32)),
            "the previous actor's name must not head this profile"
        );
        assert_eq!(
            text_of(&app, "profile-block-button"),
            "Block",
            "the previous actor's block edge must not label this toggle"
        );

        // This open's own reads still land.
        hydrate_block(&mut app, true);
        assert_eq!(text_of(&app, "profile-block-button"), "Unblock");
    }

    /// A save closes the form and re-renders the refetched header.
    #[test]
    fn save_done_closes_the_form_and_updates_the_header() {
        let mut app = self_app();
        app.profile.edit = Some(EditForm::default());
        apply_outcome(
            &mut app,
            Outcome::SaveDone {
                name: Some("Ada Lovelace".to_string()),
                pictures: HeaderPictures::default(),
                art: Box::default(),
            },
        );
        assert_eq!(
            count_id(&app, "profile-edit-form"),
            0,
            "the form closes on save"
        );
        assert_eq!(text_of(&app, "profile-handle"), "Ada Lovelace");
    }

    /// The declared `profile.state_fields` contract.
    #[test]
    fn state_json_carries_is_self_and_actor_id() {
        let app = other_app();
        let j = state_json(&app.profile);
        assert_eq!(j["is_self"], false);
        assert_eq!(j["actor_id"], "cd".repeat(32));

        let app = self_app();
        let j = state_json(&app.profile);
        assert_eq!(j["is_self"], true);
        assert_eq!(j["actor_id"], "ab".repeat(32));
    }

    // ── SELF Tiers tab §§1–5 (`tiers`) ──────────────────────────────────

    fn a_request(id: i64, tier: &str, paid: bool) -> PendingRequest {
        PendingRequest {
            home: None,
            request_id: id,
            subscriber_id: fauna_core::identity::ActorId::from_hex(&"ef".repeat(32)).unwrap(),
            tier_name: tier.to_string(),
            kind: "subscribe".to_string(),
            created_at: fauna_core::data::Timestamp(0),
            mlkem_encaps_key: None,
            payment_entitled: paid,
            extra: Default::default(),
        }
    }

    /// A SELF app sitting on the Tiers tab with one tier and one request.
    fn author_app() -> App {
        let mut app = self_app();
        app.profile.tab = Tab::Tiers;
        app.profile.author.tiers = vec![a_tier("gold", Some("$5/mo"))];
        app.profile.author.roster_tier = "gold".to_string();
        app
    }

    /// The five section landmarks + their entry-point controls paint on SELF,
    /// and NOT on OTHER (which gets the offers browse instead). This is the
    /// `is_self` branch ui.yaml declares for the tab.
    #[test]
    fn self_tiers_tab_paints_all_five_sections_and_other_does_not() {
        let app = author_app();
        let painted = ids(&app);
        for id in [
            "subscription-tiers-section",
            "subscription-tier-create-button",
            "subscription-requests-section",
            "subscription-subscribers-section",
            "subscription-subscribers-tier-select",
            "subscription-provider-section",
            "subscription-provider-add-button",
            "subscription-claim-section",
            "subscription-claim-tier-select",
            "subscription-claim-mint-button",
        ] {
            assert!(painted.contains(&id.to_string()), "SELF must paint {id}");
        }
        // The OTHER-only browse never shows on SELF.
        assert!(!painted.contains(&"subscription-offers-section".to_string()));

        let mut other = other_app();
        other.profile.tab = Tab::Tiers;
        let painted = ids(&other);
        assert!(painted.contains(&"subscription-offers-section".to_string()));
        for id in [
            "subscription-tiers-section",
            "subscription-provider-section",
            "subscription-claim-section",
        ] {
            assert!(
                !painted.contains(&id.to_string()),
                "{id} is SELF author management — never on another's profile"
            );
        }
    }

    /// The Posts tab paints none of it — the sections are scoped to the Tiers
    /// tab, so a tab that never opened cannot leak author state into the page.
    #[test]
    fn posts_tab_paints_no_author_sections() {
        let mut app = author_app();
        app.profile.tab = Tab::Posts;
        let painted = ids(&app);
        assert!(!painted.contains(&"subscription-tiers-section".to_string()));
        assert!(!painted.contains(&"subscription-claim-section".to_string()));
    }

    /// The paid badge is emitted only on an entitled row. (The flat-vs-scoped
    /// registry addressing this depends on is pinned in `automation.rs`'s
    /// `subscription_request_rows_are_flat_indexed_and_scope_their_children`,
    /// where the registry internals are in scope.)
    #[test]
    fn paid_badge_tracks_payment_entitled() {
        let mut app = author_app();
        app.profile.author.requests = vec![
            a_request(1, "gold", false),
            a_request(2, "silver", true),
            a_request(3, "bronze", false),
        ];
        assert_eq!(count_id(&app, "subscription-request-row"), 3);
        assert_eq!(
            count_id(&app, "subscription-request-paid-badge"),
            1,
            "exactly the one payment-verified request shows the badge"
        );
    }

    /// `subscription-request-busy` is painted only while an approve's
    /// mint+upload is in flight — its presence IS the assertion the e2e makes,
    /// so an idle page must not paint it.
    #[test]
    fn request_busy_marker_tracks_the_in_flight_approve() {
        let mut app = author_app();
        app.profile.author.requests = vec![a_request(1, "gold", false)];
        assert!(
            !ids(&app).contains(&"subscription-request-busy".to_string()),
            "an idle page must not paint the in-flight marker"
        );

        app.profile.author.busy = true;
        assert!(ids(&app).contains(&"subscription-request-busy".to_string()));

        // BOTH terminal outcomes clear it — a stuck marker would read
        // downstream as an approve that never finished.
        apply_outcome(&mut app, Outcome::AuthorFailed("boom".into()));
        assert!(!ids(&app).contains(&"subscription-request-busy".to_string()));
        assert_eq!(
            app.errors.get(&Page::Profile).map(String::as_str),
            Some("boom")
        );

        app.profile.author.busy = true;
        apply_outcome(
            &mut app,
            Outcome::Author(Box::new(tiers::AuthorSnapshot {
                tiers: vec![],
                requests: vec![],
                subscribers: vec![],
                roster_tier: String::new(),
                #[cfg(feature = "payments")]
                providers: vec![],
                #[cfg(feature = "payments")]
                claims: vec![],
            })),
        );
        assert!(!ids(&app).contains(&"subscription-request-busy".to_string()));
    }

    /// Opening the §1 form on a row seeds every buffer from that row, so a Save
    /// that edits one field cannot blank the others (`tiers_update` passes all
    /// six as `Some`).
    #[test]
    fn editing_a_tier_seeds_the_form_from_the_row() {
        let mut app = author_app();
        app.profile.author.tiers = vec![TierItem {
            description: Some("Gold tier".into()),
            payment_url: Some("https://pay.test/gold".into()),
            rank: 7,
            auto_approve: false,
            asking_price: fauna_protocol::subscriptions::TierAskingPrice::from_sats(500),
            ..a_tier("gold", Some("$5/mo"))
        }];
        apply_local(&mut app, Action::OpenTierForm(Some("gold".into())));

        let f = app.profile.author.form.as_ref().expect("form opens");
        assert_eq!(f.editing.as_deref(), Some("gold"));
        assert_eq!(f.name, "gold");
        assert_eq!(f.rank, "7");
        assert_eq!(f.description, "Gold tier");
        assert_eq!(f.price_hint, "$5/mo");
        assert_eq!(
            f.asking_price, "500",
            "seeded in sats, the inverse of from_sats"
        );
        assert_eq!(f.payment_url, "https://pay.test/gold");
        assert!(!f.auto_approve);
        assert!(ids(&app).contains(&"subscription-tier-form".to_string()));
        assert!(ids(&app).contains(&"subscription-tier-form-asking-price".to_string()));

        // Create opens an empty form on the same id set.
        apply_local(&mut app, Action::OpenTierForm(None));
        let f = app.profile.author.form.as_ref().expect("form opens");
        assert!(f.editing.is_none());
        assert_eq!(f.name, "");
        assert_eq!(f.asking_price, "", "an unpriced create form starts empty");
    }

    /// The §4 return-URL display is the shared `return_url` face over the
    /// creator's WEB-SITE origin — not the nest URL the webhook display beside
    /// it uses — and it tracks the kind select, placeholder and all. With no
    /// public web address it says so rather than showing a URL that cannot work.
    #[test]
    #[cfg(feature = "payments")]
    fn provider_return_url_is_built_on_the_site_origin_per_kind() {
        const ID: &str = "subscription-provider-form-return-url";
        let mut app = author_app();
        app.profile.nest_url = "https://nest.test".to_string();
        apply_local(&mut app, Action::OpenProviderForm);
        assert_eq!(
            text_of(&app, ID),
            fauna_i18n::strings::subscriptions::RETURN_URL_UNAVAILABLE,
            "no hydrated web origin → the display names the missing address"
        );

        app.settings.set_web_origin_for_test("alice", "web.test");
        let origin = crate::settings::web::site_link(&app.settings)
            .origin
            .expect("a hydrated subdomain yields an origin");
        for kind in fauna_client_payments::known_kinds() {
            apply_local(&mut app, Action::SetProviderFormKind((*kind).to_string()));
            let shown = text_of(&app, ID);
            assert_eq!(shown, fauna_client_payments::return_url(&origin, kind));
            assert!(
                !shown.contains("nest.test"),
                "built on the site origin, never the nest URL: {shown}"
            );
        }
    }

    /// The §4 webhook preview recomputes from the kind select — it is derived,
    /// never stored, so there is no stale-preview state to go wrong.
    #[test]
    #[cfg(feature = "payments")]
    fn provider_webhook_url_preview_tracks_the_kind_select() {
        let mut app = author_app();
        app.profile.nest_url = "https://nest.test".to_string();
        apply_local(&mut app, Action::OpenProviderForm);

        let kinds = fauna_client_payments::known_kinds();
        let kind = kinds.first().expect("at least one provider kind");
        apply_local(&mut app, Action::SetProviderFormKind((*kind).to_string()));

        let shown = text_of(&app, "subscription-provider-form-webhook-url");
        assert_eq!(
            shown,
            fauna_client_payments::webhook_url("https://nest.test", &"ab".repeat(32), kind),
            "the preview is the shared `webhook_url` derivation, never a restatement"
        );
        // It names the VIEWER, and it changes with the kind.
        assert!(shown.contains(&"ab".repeat(32)));
        if let Some(other_kind) = kinds.get(1) {
            apply_local(
                &mut app,
                Action::SetProviderFormKind((*other_kind).to_string()),
            );
            assert_ne!(
                text_of(&app, "subscription-provider-form-webhook-url"),
                shown
            );
        }
    }

    /// A fresh §§1–5 read closes whichever form drove it and re-points both tier
    /// pickers, so neither select can stay armed against a deleted tier.
    #[test]
    #[cfg(feature = "payments")]
    fn author_snapshot_closes_forms_and_repoints_the_tier_pickers() {
        let mut app = author_app();
        app.profile.author.claim_tier = "gone".to_string();
        apply_local(&mut app, Action::OpenTierForm(None));
        apply_local(&mut app, Action::OpenProviderForm);

        apply_outcome(
            &mut app,
            Outcome::Author(Box::new(tiers::AuthorSnapshot {
                tiers: vec![a_tier("silver", None)],
                requests: vec![],
                subscribers: vec![],
                roster_tier: "silver".to_string(),
                providers: vec![],
                claims: vec![],
            })),
        );

        let a = &app.profile.author;
        assert!(a.form.is_none(), "the §1 form closes on a landed read");
        assert!(a.provider_form.is_none(), "the §4 form closes too");
        assert_eq!(
            a.claim_tier, "silver",
            "a claim tier that no longer exists falls back to the first tier"
        );
        assert_eq!(a.roster_tier, "silver");
    }

    #[cfg(feature = "payments")]
    fn a_claim(
        code: &str,
        redeemed: bool,
        voided: bool,
    ) -> fauna_client_payments::payments::ClaimItem {
        use fauna_core::data::Timestamp;
        fauna_client_payments::payments::ClaimItem {
            code: code.to_string(),
            tier: "gold".to_string(),
            provider: "manual".to_string(),
            valid_until: None,
            created_at: Timestamp(1),
            redeemed_by: redeemed.then(|| ActorId::from_hex(&"cd".repeat(32)).unwrap()),
            redeemed_at: redeemed.then_some(Timestamp(2)),
            voided_at: voided.then_some(Timestamp(3)),
            extra: Default::default(),
        }
    }

    #[cfg(feature = "payments")]
    fn void_buttons(app: &App) -> Vec<(String, String)> {
        elements(app)
            .into_iter()
            .filter(|e| e.id == ids::SUBSCRIPTION_CLAIM_VOID_BUTTON)
            .map(|e| (e.id.clone(), e.text))
            .collect()
    }

    /// Every §5 row not yet voided carries the Void verb — redeemed rows
    /// included — and a voided row carries none.
    #[test]
    #[cfg(feature = "payments")]
    fn claim_void_button_paints_on_every_row_not_yet_voided() {
        let mut app = author_app();
        app.profile.author.claims = vec![
            a_claim("AAAA", false, false),
            a_claim("BBBB", true, false),
            a_claim("CCCC", false, true),
        ];
        let buttons = void_buttons(&app);
        assert_eq!(
            buttons.len(),
            2,
            "unredeemed + redeemed, not voided: {buttons:?}"
        );
        assert!(buttons.iter().all(|(_, t)| t == s::VOID_CLAIM));
    }

    /// The first press arms the same button in place, its text naming the
    /// consequence; only the second press on that code reaches the nest. A
    /// press on another row re-arms there, and Keep disarms.
    #[test]
    #[cfg(feature = "payments")]
    fn claim_void_arms_in_place_and_only_the_second_press_voids() {
        let mut app = with_nest(author_app());
        app.profile.author.claims =
            vec![a_claim("AAAA", false, false), a_claim("BBBB", true, false)];

        assert!(apply_local(&mut app, Action::VoidClaim("AAAA".into())).is_none());
        let texts: Vec<String> = void_buttons(&app).into_iter().map(|(_, t)| t).collect();
        assert_eq!(texts[0], s::VOID_CLAIM_CONFIRM_UNREDEEMED);
        assert_eq!(texts[1], s::VOID_CLAIM);

        // Another row's press re-arms rather than voiding the first — and a
        // redeemed code's confirm names who loses which tier.
        assert!(apply_local(&mut app, Action::VoidClaim("BBBB".into())).is_none());
        let texts: Vec<String> = void_buttons(&app).into_iter().map(|(_, t)| t).collect();
        assert_eq!(texts[0], s::VOID_CLAIM);
        assert_eq!(
            texts[1],
            s::void_claim_confirm_redeemed(&"cd".repeat(32), "gold")
        );

        assert!(apply_local(&mut app, Action::CancelVoidClaim).is_none());
        assert!(app.profile.author.claim_void_armed.is_none());

        assert!(apply_local(&mut app, Action::VoidClaim("AAAA".into())).is_none());
        let Some(Op::VoidClaim { code, .. }) =
            apply_local(&mut app, Action::VoidClaim("AAAA".into()))
        else {
            panic!("the confirmed press voids");
        };
        assert_eq!(code, "AAAA");
        assert!(app.profile.author.claim_void_armed.is_none());
    }

    /// The void is nest-arbitrated money: offline, the verb desensitizes.
    #[test]
    #[cfg(feature = "payments")]
    fn claim_void_is_online_only() {
        let kind = Action::VoidClaim("AAAA".into()).wire_kind();
        assert_eq!(kind, Some("fauna.payments.claims.void"));
        assert_eq!(
            fauna_protocol::offline_class::offline_class(kind.unwrap()),
            Some(fauna_protocol::offline_class::OfflineClass::OnlineOnly)
        );
        assert_eq!(Action::CancelVoidClaim.wire_kind(), None);
    }

    /// A landed re-read disarms: no confirm stays armed over fresh rows.
    #[test]
    #[cfg(feature = "payments")]
    fn author_snapshot_disarms_the_claim_void_confirm() {
        let mut app = author_app();
        app.profile.author.claims = vec![a_claim("AAAA", false, false)];
        apply_local(&mut app, Action::VoidClaim("AAAA".into()));
        apply_outcome(
            &mut app,
            Outcome::Author(Box::new(tiers::AuthorSnapshot {
                tiers: vec![a_tier("gold", None)],
                requests: vec![],
                subscribers: vec![],
                roster_tier: "gold".to_string(),
                providers: vec![],
                claims: vec![a_claim("AAAA", false, false)],
            })),
        );
        assert!(app.profile.author.claim_void_armed.is_none());
    }

    /// An auto-minted per-post unlock tier, as `tiers.list` hands it back.
    fn an_unlock_tier(name: &str) -> TierItem {
        TierItem {
            unlocks_post: Some("ab".repeat(32)),
            auto_approve: false,
            ..a_tier(name, Some("$3"))
        }
    }

    /// The options a select paints, by id — §3/§4/§5 each offer a tier picker
    /// and the whole point of the fix below is *which* tiers reach them.
    ///
    /// Only the §§4–5 picker test reads it, so it follows that test's gate.
    #[cfg(feature = "payments")]
    fn select_options(app: &App, id: &str) -> Vec<String> {
        elements(app)
            .into_iter()
            .find(|e| e.id == id)
            .and_then(|e| match e.role {
                crate::element::Role::Select { options, .. } => Some(options),
                _ => None,
            })
            .unwrap_or_default()
    }

    /// Per-post pay-to-unlock tiers are filtered out of §1: they are sold on the
    /// post, not in the management list (`monetization.md` § Per-post
    /// pay-to-unlock), so an author who sold three posts does not find three
    /// phantom tiers here.
    ///
    /// ⚠ This asserts the RENDER. Its previous body asserted
    /// `unlock.unlocks_post.is_some()` on a struct it had just built — a
    /// tautology that passed no matter what §1 painted, and so could never have
    /// caught the inverse bug the pickers below had (conventions rule 6's
    /// sibling: a pin that cannot fail is not coverage).
    #[test]
    fn unlock_tiers_are_excluded_from_the_management_list() {
        let mut app = author_app();
        app.profile.author.tiers = vec![a_tier("gold", None), an_unlock_tier("post-unlock-abc")];

        assert_eq!(
            count_id(&app, "subscription-tier-row"),
            1,
            "§1 lists only the manageable tier, not the auto-minted unlock one"
        );
        assert_eq!(text_of(&app, "subscription-tier-name"), "gold");
    }

    /// …and the §3/§4/§5 pickers keep them — the other half of the ratified
    /// split (`monetization.md` § Per-post pay-to-unlock → gap (2b): the §1
    /// exclusion "leaves the unfiltered `tiers` list feeding §3/§4/§5
    /// untouched — a designated tier still needs a claim minted / subscribers
    /// viewed against it"). linux/web/android/apple all filter at §1's render
    /// only; tui filtered at the READ, starving all three pickers.
    #[test]
    #[cfg(feature = "payments")]
    fn the_three_tier_pickers_still_offer_a_designated_unlock_tier() {
        let mut app = author_app();
        app.profile.author.tiers = vec![a_tier("gold", None), an_unlock_tier("post-unlock-abc")];
        apply_local(&mut app, Action::OpenProviderForm);

        for id in [
            "subscription-subscribers-tier-select",
            "subscription-provider-form-tier-map",
            "subscription-claim-tier-select",
        ] {
            let options = select_options(&app, id);
            assert!(
                options.iter().any(|o| o == "post-unlock-abc"),
                "{id} must offer the designated tier — the author still mints \
                 claims against it and views its buyers; got {options:?}"
            );
        }
    }

    /// The §3 roster default for the shape `test_sell_post.py`'s buyer leg
    /// creates: a seller whose ONLY tier is the post's auto-minted unlock tier.
    /// Picking off a §1-filtered list left `roster_tier` EMPTY here, so
    /// `fetch_author` skipped `subscribers_list` entirely and §3 read zero
    /// buyers no matter what the nest had granted — with no error surfaced.
    #[test]
    fn the_roster_defaults_to_the_unlock_tier_when_it_is_the_only_one() {
        let only_sold = vec![an_unlock_tier("post-unlock-abc")];
        assert_eq!(
            tiers::pick_tier(&only_sold, ""),
            "post-unlock-abc",
            "a seller who has only sold posts must still see that post's buyers"
        );

        // The author's own pick survives a re-read, designated or not.
        let both = vec![a_tier("gold", None), an_unlock_tier("post-unlock-abc")];
        assert_eq!(
            tiers::pick_tier(&both, "post-unlock-abc"),
            "post-unlock-abc"
        );
        // A pick that no longer exists falls back to the first tier.
        assert_eq!(tiers::pick_tier(&both, "deleted"), "gold");
        assert_eq!(tiers::pick_tier(&[], "gold"), "");
    }

    /// An offer's external payment link paints only when the tier carries one,
    /// and a non-https link is refused rather than opened (author-supplied
    /// content — the feed's `gated-post-payment-link` guard).
    #[cfg(feature = "payments")]
    #[test]
    fn offer_payment_link_is_conditional_and_https_only() {
        let mut app = other_app();
        app.profile.tab = Tab::Tiers;
        app.profile.offers = vec![a_tier("gold", None)];
        assert!(!ids(&app).contains(&"subscription-offer-payment-link".to_string()));

        app.profile.offers = vec![TierItem {
            payment_url: Some("https://pay.test/gold".into()),
            ..a_tier("gold", None)
        }];
        assert!(ids(&app).contains(&"subscription-offer-payment-link".to_string()));

        apply_local(&mut app, Action::OpenPaymentLink("http://evil.test".into()));
        assert!(
            app.errors.contains_key(&Page::Profile),
            "a non-https payment link must be refused, not opened"
        );
    }

    /// The repeatable-links sub-list landmark paints with the edit form (the
    /// ui.yaml `profile-edit-link-list` component).
    #[test]
    fn edit_form_paints_the_link_list_landmark() {
        let mut app = self_app();
        assert!(!ids(&app).contains(&"profile-edit-link-list".to_string()));
        apply_local(&mut app, Action::OpenEdit);
        assert!(ids(&app).contains(&"profile-edit-link-list".to_string()));
    }

    // ── The private section (profile.md § The private section) ─────────────

    /// An OTHER profile over a live overlay projection holding `overlay` for
    /// the viewed person — the shape `App::open_profile` wires.
    fn other_with_overlay(
        overlay: Option<fauna_core::contact_overlay::ContactOverlay>,
    ) -> (App, Arc<fauna_conversations::contacts::ContactsCache>) {
        let mut app = other_app();
        let cache = fauna_conversations::contacts::ContactsCache::new();
        if let Some(o) = overlay {
            cache.replace([("cd".repeat(32), o)].into_iter().collect());
        }
        app.profile.overlays = Some(Arc::clone(&cache));
        (app, cache)
    }

    fn overlay_with(nick: &str, labels: &[&str]) -> fauna_core::contact_overlay::ContactOverlay {
        use fauna_core::contact_overlay::{ContactOverlay, Register, Stamp, fold_label};
        let reg = |v: &str| Register {
            stamp: Stamp::new(1, [1; 32]),
            value: Some(v.to_string()),
        };
        ContactOverlay {
            nickname: reg(nick),
            labels: labels.iter().map(|l| (fold_label(l), reg(l))).collect(),
            ..Default::default()
        }
    }

    fn texts_of(app: &App, id: &str) -> Vec<String> {
        elements(app)
            .into_iter()
            .filter(|e| e.id == id)
            .map(|e| e.text)
            .collect()
    }

    #[test]
    fn the_private_section_paints_on_other_only() {
        let (app, _) = other_with_overlay(None);
        let got = ids(&app);
        for required in [
            "profile-private-section",
            "profile-nickname-field",
            "profile-notes-field",
            "profile-label-list",
            "profile-label-field",
            "profile-label-add-button",
            "profile-private-save-button",
        ] {
            assert!(
                got.contains(&required.to_string()),
                "OTHER paints {required}"
            );
        }
        assert!(
            !got.contains(&"profile-public-name".to_string()),
            "no nickname, no public-name line"
        );
        let own = ids(&self_app());
        assert!(
            !own.iter().any(|id| id.starts_with("profile-private")
                || id.starts_with("profile-label")
                || id == "profile-nickname-field"),
            "never on the viewer's own profile"
        );
    }

    #[test]
    fn a_nickname_heads_the_profile_with_the_public_name_beneath() {
        let (mut app, _) = other_with_overlay(Some(overlay_with("Mum", &["Family"])));
        app.profile.header_name = Some("Ada Lovelace".to_string());
        assert_eq!(text_of(&app, "profile-handle"), "Mum");
        assert_eq!(text_of(&app, "profile-public-name"), "Ada Lovelace");
        assert_eq!(text_of(&app, "profile-nickname-field"), "Mum");
        assert_eq!(texts_of(&app, "profile-label-chip"), vec!["Family"]);
    }

    #[test]
    fn untouched_fields_read_live_and_staged_ones_hold() {
        let (mut app, cache) = other_with_overlay(None);
        assert_eq!(text_of(&app, "profile-nickname-field"), "");
        // A sibling device's edit arrives before this user touched anything.
        cache.replace(
            [("cd".repeat(32), overlay_with("Mum", &[]))]
                .into_iter()
                .collect(),
        );
        assert_eq!(text_of(&app, "profile-nickname-field"), "Mum");
        // Once the user edits, the staged value holds against later loads.
        set_field(
            &mut app.profile,
            ProfileField::PrivateNotes,
            "likes tea".into(),
        );
        cache.replace(
            [("cd".repeat(32), overlay_with("Mother", &[]))]
                .into_iter()
                .collect(),
        );
        assert_eq!(text_of(&app, "profile-notes-field"), "likes tea");
        assert_eq!(
            text_of(&app, "profile-nickname-field"),
            "Mum",
            "the staged form snapshots every field at the first edit"
        );
    }

    #[test]
    fn labels_stage_through_the_shared_validator() {
        let (mut app, _) = other_with_overlay(None);
        set_field(
            &mut app.profile,
            ProfileField::PrivateLabel,
            "  Book   club ".into(),
        );
        assert!(apply_local(&mut app, Action::AddPrivateLabel).is_none());
        set_field(
            &mut app.profile,
            ProfileField::PrivateLabel,
            "book CLUB".into(),
        );
        apply_local(&mut app, Action::AddPrivateLabel);
        assert_eq!(
            texts_of(&app, "profile-label-chip"),
            vec!["book CLUB"],
            "one label per folded form, displayed as last written"
        );
        assert_eq!(text_of(&app, "profile-label-field"), "");
        // An empty label is refused onto error-message, nothing staged.
        apply_local(&mut app, Action::AddPrivateLabel);
        assert!(app.errors.contains_key(&Page::Profile));
        apply_local(&mut app, Action::RemovePrivateLabel(0));
        assert!(texts_of(&app, "profile-label-chip").is_empty());
    }

    #[test]
    fn typing_a_label_prefix_offers_the_vocabulary_and_a_pick_stages_it() {
        let (mut app, cache) = other_with_overlay(Some(overlay_with("Mum", &["Friends"])));
        // Another person's labels join the derived vocabulary.
        let mut all: std::collections::BTreeMap<_, _> = [
            ("cd".repeat(32), overlay_with("Mum", &["Friends"])),
            ("ab".repeat(32), overlay_with("Bo", &["Family", "Fencing"])),
        ]
        .into_iter()
        .collect();
        cache.replace(std::mem::take(&mut all));
        assert!(
            texts_of(&app, "profile-label-suggestion").is_empty(),
            "nothing is offered until something is typed"
        );
        set_field(&mut app.profile, ProfileField::PrivateLabel, "f".into());
        assert_eq!(
            texts_of(&app, "profile-label-suggestion"),
            vec!["Family", "Fencing"],
            "labels already staged for this person are left out"
        );
        apply_local(&mut app, Action::PickLabelSuggestion(1));
        assert_eq!(
            texts_of(&app, "profile-label-chip"),
            vec!["Friends", "Fencing"]
                .into_iter()
                .map(String::from)
                .collect::<Vec<_>>()
        );
        assert_eq!(text_of(&app, "profile-label-field"), "");
        assert!(texts_of(&app, "profile-label-suggestion").is_empty());
    }

    #[test]
    fn a_save_without_a_store_keeps_the_staged_edits() {
        let (mut app, _) = other_with_overlay(None);
        // Nothing staged: Save is a no-op.
        assert!(apply_local(&mut app, Action::SavePrivate).is_none());
        assert!(!app.errors.contains_key(&Page::Profile));
        set_field(
            &mut app.profile,
            ProfileField::PrivateNickname,
            "Mum".into(),
        );
        assert!(apply_local(&mut app, Action::SavePrivate).is_none());
        assert!(
            app.errors.contains_key(&Page::Profile),
            "the refusal is shown"
        );
        assert_eq!(
            text_of(&app, "profile-nickname-field"),
            "Mum",
            "staging kept"
        );
        // A bounds refusal is shown the same way.
        set_field(
            &mut app.profile,
            ProfileField::PrivateNickname,
            "x".repeat(65),
        );
        app.errors.clear();
        apply_local(&mut app, Action::SavePrivate);
        assert!(app.errors.contains_key(&Page::Profile));
    }

    #[test]
    fn a_landed_save_drops_the_staging_for_its_own_person_only() {
        let (mut app, _) = other_with_overlay(None);
        set_field(
            &mut app.profile,
            ProfileField::PrivateNickname,
            "Mum".into(),
        );
        apply_outcome(
            &mut app,
            Outcome::PrivateSaved {
                actor: "ef".repeat(32),
            },
        );
        assert_eq!(text_of(&app, "profile-nickname-field"), "Mum");
        apply_outcome(
            &mut app,
            Outcome::PrivateSaved {
                actor: "cd".repeat(32),
            },
        );
        assert_eq!(
            text_of(&app, "profile-nickname-field"),
            "",
            "reads the (here empty) projection again"
        );
    }
}

#[cfg(test)]
mod posts_tab_tests {
    //! The Posts tab ([`posts`]; `profile.md` § Layout & flow → *The Posts tab*).
    use super::*;
    use crate::app::tests::{authed_app, test_session};
    use fauna_feed::test_support::{TestPostSpec, feed_snapshot_with_posts};
    use fauna_feed::{FeedManager, FeedSnapshot, FeedStatus};

    const AUTHOR: &str = "cd";

    /// An authed app with a timeline manager, on OTHER's profile, the Posts
    /// tab's window minted the way the open mints it.
    fn posts_app() -> App {
        let mut app = authed_app();
        app.feed.manager = Some(Arc::new(FeedManager::new(test_session().client, [7u8; 32])));
        app.page = Page::Profile;
        app.profile.self_actor_id = "ab".repeat(32);
        app.profile.viewing = Some(AUTHOR.repeat(32));
        assert!(
            posts::open(&mut app).is_some(),
            "the open reads the first page"
        );
        app
    }

    fn window(app: &App) -> Arc<crate::feed::CliFeedManager> {
        app.profile.posts.clone().expect("the open minted a window")
    }

    fn post(id: &str, body: &str) -> TestPostSpec {
        TestPostSpec {
            post_id: id.repeat(32),
            author: AUTHOR.repeat(32),
            body: body.to_string(),
            ..Default::default()
        }
    }

    fn texts(app: &App, id: &str) -> Vec<String> {
        elements(app)
            .into_iter()
            .filter(|e| e.id == id && e.path.is_empty())
            .map(|e| e.text)
            .collect()
    }

    fn gesture_of(app: &App, id: &str, index: usize) -> Gesture {
        let el = elements(app)
            .into_iter()
            .filter(|e| e.id == id)
            .nth(index)
            .unwrap_or_else(|| panic!("{id}[{index}] painted"));
        match el.role {
            crate::element::Role::Button(g) => g,
            _ => panic!("{id}[{index}] is a button"),
        }
    }

    /// The window is the viewed actor's, never the timeline: every open mints
    /// it with the page's actor.
    #[test]
    fn the_open_mints_an_author_window_for_the_viewed_actor() {
        let app = posts_app();
        let w = window(&app);
        assert_eq!(
            w.snapshot().author_selected.as_deref(),
            Some(AUTHOR.repeat(32).as_str())
        );
        let timeline = app.feed.manager.clone().unwrap();
        assert!(
            !Arc::ptr_eq(&w, &timeline),
            "a second manager, not the timeline"
        );
        assert_eq!(timeline.snapshot().author_selected, None);
    }

    /// SELF reads its own posts the same way — the viewer's own id, never the
    /// body enumeration (`profile.md` § The Posts tab → *Where the data comes
    /// from*).
    #[test]
    fn the_self_tab_is_the_same_read_with_the_viewers_own_id() {
        let mut app = posts_app();
        app.profile.viewing = None;
        posts::open(&mut app);
        assert_eq!(
            window(&app).snapshot().author_selected.as_deref(),
            Some("ab".repeat(32).as_str())
        );
    }

    /// Re-opening the tab reads afresh: a new window, not the cached one.
    #[test]
    fn every_open_of_the_tab_mints_a_fresh_window() {
        let mut app = posts_app();
        let first = window(&app);
        app.profile.tab = Tab::Tiers;
        assert!(apply_local(&mut app, Action::ShowPosts).is_some());
        assert!(!Arc::ptr_eq(&first, &window(&app)));
        assert_eq!(app.profile.tab, Tab::Posts);
    }

    /// The rows paint as the feed paints them — `post-card`s in the window's
    /// (newest-first) order, with their feed children — under the tab's section.
    #[test]
    fn the_window_paints_feed_cards_in_its_order() {
        let app = posts_app();
        window(&app).set_feed_snapshot_for_test(feed_snapshot_with_posts(vec![
            post("01", "newer"),
            post("02", "older"),
        ]));
        assert_eq!(texts(&app, ids::PROFILE_POSTS_SECTION).len(), 1);
        assert_eq!(texts(&app, ids::POST_CARD), vec!["newer", "older"]);
        let likes = elements(&app)
            .into_iter()
            .filter(|e| e.id == ids::FEED_LIKE_BUTTON)
            .count();
        assert_eq!(likes, 2, "each card carries the feed's interaction bar");
        assert!(texts(&app, ids::PROFILE_POSTS_EMPTY).is_empty());
    }

    /// `profile-posts-empty` iff the window is `Loaded` and empty; nothing while
    /// the first read is in flight.
    #[test]
    fn the_empty_state_follows_the_shared_rule() {
        let app = posts_app();
        assert_eq!(window(&app).snapshot().status, FeedStatus::Loading);
        assert!(texts(&app, ids::PROFILE_POSTS_EMPTY).is_empty(), "loading");
        assert_eq!(texts(&app, ids::PROFILE_POSTS_SECTION).len(), 1);

        window(&app).set_feed_snapshot_for_test(FeedSnapshot {
            status: FeedStatus::Loaded,
            ..FeedSnapshot::default()
        });
        assert_eq!(
            texts(&app, ids::PROFILE_POSTS_EMPTY),
            vec![fauna_i18n::strings::profile::NO_POSTS.to_string()]
        );
        assert!(texts(&app, ids::POST_CARD).is_empty());
    }

    /// The Tiers tab paints none of it.
    #[test]
    fn the_tiers_tab_paints_no_posts_section() {
        let mut app = posts_app();
        app.profile.tab = Tab::Tiers;
        assert!(texts(&app, ids::PROFILE_POSTS_SECTION).is_empty());
    }

    /// Activating a card is the search door: the timeline manager resolves the
    /// post and the feed page's `post_detail` opens on it.
    #[test]
    fn a_card_opens_the_feed_post_detail_through_the_door() {
        let mut app = posts_app();
        window(&app).set_feed_snapshot_for_test(feed_snapshot_with_posts(vec![post("01", "hi")]));
        let Gesture::Profile(action) = gesture_of(&app, ids::POST_CARD, 0) else {
            panic!("the card's gesture is re-routed through the profile page");
        };
        let Some(Op::Feed(op)) = apply_local(&mut app, action) else {
            panic!("the door returns the feed's open op");
        };
        assert!(matches!(*op, crate::feed::Op::OpenPostDetail { .. }));
        assert_eq!(app.page, Page::Feed);
        assert_eq!(
            app.feed.mode,
            crate::feed::Mode::PostDetail("01".repeat(32))
        );
    }

    /// A card's `video-thumbnail` is never a dead button on the tab: the
    /// handoff's confirm lives on the feed page only, so activating it opens
    /// the post's detail there, where the thumbnail runs the handoff.
    #[test]
    fn a_cards_video_opens_the_feed_post_detail_where_the_handoff_runs() {
        let mut app = posts_app();
        let mut snapshot = feed_snapshot_with_posts(vec![post("01", "a clip")]);
        snapshot.posts[0]
            .document
            .blocks
            .push(fauna_core::render::RenderBlock::Video {
                hash: "5e".repeat(32),
                alt: String::new(),
            });
        window(&app).set_feed_snapshot_for_test(snapshot);
        let Gesture::Profile(action) = gesture_of(&app, ids::VIDEO_THUMBNAIL, 0) else {
            panic!("the thumbnail's gesture is re-routed through the profile page");
        };
        let Some(Op::Feed(op)) = apply_local(&mut app, action) else {
            panic!("the thumbnail opens the post through the door");
        };
        assert!(matches!(*op, crate::feed::Op::OpenPostDetail { .. }));
        assert_eq!(app.page, Page::Feed);
        assert_eq!(
            app.feed.mode,
            crate::feed::Mode::PostDetail("01".repeat(32))
        );
        assert!(app.feed.video_confirm.is_none(), "nothing armed on the way");
    }

    /// A like on a profile card acts on the window — the manager its row came
    /// from, which is what moves the count the tab paints.
    #[test]
    fn a_cards_interaction_acts_on_the_window() {
        let mut app = posts_app();
        window(&app).set_feed_snapshot_for_test(feed_snapshot_with_posts(vec![post("01", "hi")]));
        let Gesture::Profile(action) = gesture_of(&app, ids::FEED_LIKE_BUTTON, 0) else {
            panic!("re-routed");
        };
        let Some(Op::Feed(op)) = apply_local(&mut app, action) else {
            panic!("an interact op");
        };
        let crate::feed::Op::Interact { manager, .. } = *op else {
            panic!("the like is an interact");
        };
        assert!(Arc::ptr_eq(&manager, &window(&app)));
        assert_eq!(app.page, Page::Profile, "the like stays on the tab");
    }

    /// The card's ⋯ opens the feed's own menu here, on the Profile page, and
    /// its own-post answer is the window's: the post is the viewer's and only
    /// the window holds it, so the delete verb paints — and arms — off the
    /// window. The feed page, whose timeline does not hold the post, paints no
    /// menu for it.
    #[test]
    fn a_cards_actions_menu_opens_on_the_tab_over_the_window() {
        let mut app = posts_app();
        app.profile.viewing = None;
        posts::open(&mut app);
        let own = TestPostSpec {
            author: crate::app::tests::test_actor_id(),
            ..post("01", "mine")
        };
        window(&app).set_feed_snapshot_for_test(feed_snapshot_with_posts(vec![own]));
        let Gesture::Profile(action) = gesture_of(&app, ids::FEED_POST_ACTIONS_BUTTON, 0) else {
            panic!("the ⋯ is re-routed through the profile page");
        };
        apply_local(&mut app, action);
        assert_eq!(app.page, Page::Profile, "the menu opens in place");
        assert_eq!(
            app.feed.actions_open.as_deref(),
            Some("01".repeat(32).as_str())
        );
        assert_eq!(texts(&app, ids::FEED_POST_ACTIONS_MENU).len(), 1);

        let Gesture::Profile(delete) = gesture_of(&app, ids::FEED_POST_DELETE_BUTTON, 0) else {
            panic!("the window's own-post answer paints delete, re-routed");
        };
        apply_local(&mut app, delete);
        assert_eq!(
            app.feed.delete_confirm.as_deref(),
            Some("01".repeat(32).as_str())
        );
        let Gesture::Profile(confirm) = gesture_of(&app, ids::FEED_POST_DELETE_CONFIRM_BUTTON, 0)
        else {
            panic!("the armed confirm, re-routed");
        };
        let Some(Op::Feed(op)) = apply_local(&mut app, confirm) else {
            panic!("a delete op");
        };
        let crate::feed::Op::DeletePost { manager, .. } = *op else {
            panic!("the confirm deletes");
        };
        assert!(Arc::ptr_eq(&manager, &window(&app)), "on the window");

        apply_local(
            &mut app,
            Action::Post(crate::feed::Action::OpenPostActions("01".repeat(32))),
        );
        assert!(
            !crate::feed::elements(&app)
                .iter()
                .any(|e| e.id == ids::FEED_POST_ACTIONS_MENU),
            "the timeline does not hold the post, so the feed paints no menu"
        );
    }

    /// The card's reply opens `feed-reply-dialog` here, on the Profile page,
    /// targeting that post — and the submit replies through the window.
    #[test]
    fn a_cards_reply_opens_the_dialog_on_the_tab_and_submits_on_the_window() {
        let mut app = posts_app();
        window(&app).set_feed_snapshot_for_test(feed_snapshot_with_posts(vec![post("01", "hi")]));
        let Gesture::Profile(action) = gesture_of(&app, ids::FEED_REPLY_BUTTON, 0) else {
            panic!("the reply is re-routed through the profile page");
        };
        apply_local(&mut app, action);
        assert_eq!(app.page, Page::Profile, "the dialog opens in place");
        assert_eq!(
            texts(&app, ids::FEED_REPLY_DIALOG),
            vec![fauna_i18n::strings::feed::post::replying_to_user(
                &AUTHOR.repeat(32)
            )]
        );

        crate::feed::set_field(
            &mut app.feed,
            crate::feed::FeedField::ReplyText,
            "agreed".into(),
        );
        let Gesture::Profile(submit) = gesture_of(&app, ids::FEED_REPLY_SUBMIT_BUTTON, 0) else {
            panic!("the submit is re-routed");
        };
        let Some(Op::Feed(op)) = apply_local(&mut app, submit) else {
            panic!("a reply op");
        };
        let crate::feed::Op::Reply {
            manager, post_id, ..
        } = *op
        else {
            panic!("the submit replies");
        };
        assert!(Arc::ptr_eq(&manager, &window(&app)), "on the window");
        assert_eq!(post_id, "01".repeat(32));
    }

    /// The window's read failure lands on the page's `error-message`.
    #[test]
    fn a_window_error_lands_on_the_page_error_line() {
        let app = posts_app();
        assert_eq!(app.error_line_text(), None);
        window(&app).inject_error_for_test(fauna_core::localized::LocalizedText::key_arg(
            "feed.error_load",
            "message",
            "nest rejected fauna.feed.author.posts",
        ));
        let text = app.error_line_text().expect("surfaces on error-message");
        assert!(
            text.contains("nest rejected fauna.feed.author.posts"),
            "{text:?}"
        );
    }

    /// A fresh open drops the previous actor's window.
    #[test]
    fn a_fresh_open_drops_the_previous_window() {
        let mut app = posts_app();
        app.profile.reset_for_open();
        assert!(app.profile.posts.is_none());
    }
}
