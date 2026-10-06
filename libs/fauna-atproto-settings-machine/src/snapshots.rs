//! The page-level renderable [`AtprotoSettingsSnapshot`] + its row types.
//! Clients read a fresh copy on every observer tick and render the whole
//! ATProto settings login-plane surface off it; they never see the
//! internal state. Mirrors `fauna_labeler_catalog_machine::snapshots`.
//!
//! **No secret ever appears here.** The passively-rendered snapshot carries
//! credential *metadata* only; the credential secret is returned exclusively by
//! the explicit, on-demand `mint` / `reveal_secret` accessors as a
//! `SecretString`. That is the shipped mail rule
//! (`fauna_client_mail_settings::machine::reveal_credential_secret` — "the
//! passively rendered state never carries secrets"), and it matters more here:
//! a snapshot is cloned on every tick into every observing view, so a secret in
//! it would be copied into an unbounded number of places the zeroizing type can
//! no longer discipline.

use serde::{Deserialize, Serialize};

use fauna_core::localized::LocalizedText;

/// One minted app credential (`atproto-app-credential-item`). Transcribes the
/// `fauna.bridges.atproto.list_app_credentials` row
/// (`fauna_protocol::atproto_pds::AppCredentialInfo`) — never verifier
/// material, which the nest holds and no client needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AppCredentialRow {
    /// Kebab-case id, stable across devices — the same id the nest's
    /// `atproto_app_credentials` row and the local
    /// `fauna.state.atproto` row carry.
    pub credential_id: String,
    /// The user-supplied label ("Ivory", "Graysky", …).
    pub label: String,
    /// Whether the credential carries the DM-privileged scope (the ecosystem's
    /// `com.atproto.appPassPrivileged` split).
    pub dm_allowed: bool,
    /// Epoch **milliseconds** (the `fauna.bridges.atproto.*` wire convention;
    /// `atproto_pds.rs` module doc), verbatim from the nest.
    pub created_at_millis: i64,
    /// Epoch milliseconds of the last `createSession` that used this
    /// credential; `None` until it is first used.
    pub last_used_at_millis: Option<i64>,
    /// Whether this device currently holds the recoverable secret, i.e. whether
    /// `atproto-app-credential-reveal` should render for the row.
    ///
    /// The nest is the authority on *which credentials exist* and can never
    /// recover a secret (D3 custody split — it stores only the Argon2id PHC
    /// verifier), so this flag is exactly "a matching
    /// `fauna.state.atproto` row is present locally". `false`
    /// is a normal, non-error state: the row was minted on a sibling device
    /// whose `fauna.state.atproto` row has not synced here yet. Recovery is revoke +
    /// re-mint, never a nest-side lookup.
    pub revealable: bool,
}

/// The OAuth half of a connected-apps row — what the user actually approved,
/// joined onto the session that carries its liveness. Transcribes
/// `fauna_protocol::atproto_pds::AtprotoGrantInfo`.
///
/// **Nested `Option` rather than flat fields on [`AtprotoSessionRow`], and that
/// is the point.** An app-credential session has no client id and no approved
/// scope set; flattening these onto every row would let a client render a scope
/// list for a session nobody ever consented to. `Some` here means "an OAuth
/// grant stands behind this session", which is exactly the distinction the row
/// is trying to draw.
///
/// **Deliberately carries no `created_at`/`expires_at`.** Both already live on
/// the session row and mean the same thing there (one identifier joins them —
/// see [`AtprotoSessionRow::grant`]), so duplicating them would create two
/// sources for one fact and a way for them to disagree. *The grant is the
/// identity; the session is the liveness.*
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AtprotoGrantRow {
    /// The client-id URL, rendered **verbatim** beside the resolved name: it is
    /// the client's self-authenticating identity, while the name is only what a
    /// document at that URL claimed. Nothing derives an origin, a display name
    /// or a fetch target from it.
    pub client_id: String,
    /// The client's name from its *resolved* metadata document, when it
    /// published one — **control-character-stripped by the machine** before it
    /// reaches any painter (see [`AtprotoSessionRow::grant`]).
    pub client_name: Option<String>,
    /// What the user approved, one human-readable line per scope, in the
    /// grant's order.
    ///
    /// Composed from `fauna_bridge_atproto::authz::describe_scope` — the **one**
    /// owner of scope wording, shared with the consent card and the browser
    /// consent page. A user comparing what they approved against what this row
    /// says they approved must not be reading two different vocabularies, so
    /// never hand-write a second wording here or in a client.
    pub scope_descriptions: Vec<String>,
    /// The permission sets this grant was made from, frozen at the ceremony —
    /// the *same* provenance [`ConsentCardRow::sets`] showed, composed by the
    /// same function.
    ///
    /// Without it this row could only say *what* an app may do, never *why it
    /// may*: a user who approved a named bundle sees a flat list of scopes they
    /// never asked for individually, with nothing to connect the two. Empty for
    /// a grant that named no set, and for every grant recorded before PS-b.
    pub sets: Vec<ConsentSetRow>,
    /// Epoch milliseconds of the last observed use — **advisory, not
    /// forensics** (`atproto-pds-full.md` D10 § Audit). The nest stamps this
    /// inside the refresh-rotation transaction, the one moment it observes the
    /// grant in use; access-token calls verify bridge-side and never reach the
    /// nest. So a recent value proves use and an old one proves nothing, and a
    /// surface rendering it must not imply otherwise.
    pub last_used_at_millis: Option<i64>,
}

/// One connected external app (`atproto-connected-app-item`) — a live session,
/// or an OAuth grant whose session was suspended out from under it. Transcribes
/// `fauna_protocol::atproto_pds::AtprotoSessionInfo`, widened by the grant.
///
/// **The name says "session"; the row means "connection".** For the
/// app-credential plane those are the same thing. For the OAuth plane they are
/// not, and `ui/atproto.md`'s downward matrix (`:111`) is where they part: a
/// step-down (or `delete_presence`'s teardown) revokes every session while
/// deliberately leaving grant rows at rest — "kept, listed, individually
/// revocable; stepping back up restores usability". So an OAuth row can outlive
/// its session, and the field this doc already stated as the rule —
/// *the grant is the identity; the session is the liveness* — is what decides
/// which half supplies which value below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AtprotoSessionRow {
    /// Hex-encoded session id — the opaque handle `revoke_session` takes. Hex
    /// (not raw bytes) so the whole snapshot stays a plain FFI record and the
    /// per-app views can key list rows on it directly.
    pub session_id_hex: String,
    /// Which auth plane minted it: `"app_credential"` today; the OAuth plane
    /// joins at F4. Rendered verbatim — clients never re-derive it.
    pub plane: String,
    /// The app credential this session was minted from, when the plane has one.
    pub credential_id: Option<String>,
    /// Client-supplied note (e.g. the app's own name), when present.
    pub client_note: Option<String>,
    /// Epoch milliseconds.
    pub created_at_millis: i64,
    /// Epoch milliseconds of the last successful refresh; `None` if never
    /// refreshed.
    pub last_refreshed_at_millis: Option<i64>,
    /// Epoch milliseconds at which the refresh token expires — `None` exactly
    /// when there is no live session to expire, i.e. a [`suspended`](Self::suspended) row.
    ///
    /// **Optional rather than falling back to the grant's own horizon**, which
    /// was the tempting shape and is the wrong one: `record_atproto_oauth_grant`
    /// takes `session_expires_at` and `grant_expires_at` as two separate
    /// arguments meaning two different things, and the grant's is itself
    /// optional (open-ended for a confidential client). Substituting one for
    /// the other would render a date that is not this row's, on a row that is
    /// not working — an expiry countdown reads as "it works until then", which
    /// is the precise opposite of what a suspended row must say.
    pub expires_at_millis: Option<i64>,
    /// The OAuth grant behind this session, when there is one — `Some` exactly
    /// for `plane == "oauth"` rows.
    ///
    /// The join costs no nest work and needs no join table: `record_grant`
    /// writes the grant row and its session family in **one transaction with
    /// one identifier** (`atproto_oauth_grants.grant_id` *is*
    /// `atproto_sessions.session_id`), so the machine joins
    /// [`session_id_hex`](Self::session_id_hex) to the hex of
    /// `AtprotoGrantInfo::grant_id` and that is the whole mechanism. The same
    /// identity is why revoking needs no `revoke_grant`: `revoke_session` over
    /// this row's id cascades to the grant.
    ///
    /// **Composed here, in the shared machine, never in a per-app painter.**
    /// `client_name` is attacker-supplied text (it comes from a document at a
    /// URL the requesting client chose), and every app paints this row as one
    /// label; stripping it at composition is what makes the fence hold for all
    /// seven apps at once, instead of arming the trap six more times. A per-app
    /// *structural* fix is not sufficient — a name carrying fifty newlines
    /// still paints fifty rows.
    pub grant: Option<AtprotoGrantRow>,
    /// The account's login plane was suspended while this grant was left at
    /// rest — the app cannot currently act, but the user's approval stands and
    /// is still individually revocable from this row.
    ///
    /// Derived nest-side, never stored (`atproto-oauth-provider.md:127`): a
    /// grant reads as suspended exactly while it is unrevoked and its same-id
    /// session is dead, so a fresh session replacing it un-suspends the row
    /// with no separate step and no state to get stuck in.
    ///
    /// Always `false` on the app-credential plane, which has no grant row at
    /// all — that plane's durable surface is the credential list beside this
    /// one, which a step-down leaves standing for the same reason.
    pub suspended: bool,
}

/// One pending OAuth consent request as the approval card renders it
/// (`atproto-consent-card`; `atproto-pds-full.md` § F4 detail — *Consent
/// ceremony wire flow*).
///
/// The card shows exactly three things — the client identity, the scope list in
/// human-readable form, and the binding code — and the user's whole job is to
/// check that the code matches the one their browser is showing before they
/// approve. That comparison is what binds *this* browser tab to *this* approval,
/// so a phished consent push (one the user never started) is visibly wrong on
/// the card.
///
/// **Deliberately carries no expiry.** A resolution is reported even past
/// `expires_at`, so the honest "this request just expired" can only ever come
/// from the *resolve reply* — never from a client-side pre-check that greys the
/// approve control while the row is in fact still answerable. Withholding the
/// timestamp is what makes that ruling hold by construction rather than by
/// everyone remembering it; a surface that later wants to render a countdown
/// adds the field back deliberately, with that ruling in hand.
///
/// **`logo_uri` is deliberately absent too**, and that decision is upstream of
/// this record: it never crosses the wire at all. Loading an attacker-named URL
/// inside the user's app would disclose their address to whoever published the
/// client's metadata document, and the consent card is exactly where an attacker
/// controls that URL.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConsentCardRow {
    /// Hex-encoded consent id — the opaque handle
    /// [`AtprotoSettingsMachine::resolve_consent`](crate::AtprotoSettingsMachine::resolve_consent)
    /// takes. Hex (not raw bytes) so the whole snapshot stays a plain FFI record
    /// and per-app views can key list rows on it directly, the same
    /// convention [`AtprotoSessionRow::session_id_hex`] already uses.
    pub consent_id_hex: String,
    /// The binding code (`atproto-consent-code`) the user compares against
    /// their browser. **Minted by the nest**, so this value and the one the
    /// `/oauth/authorize` page renders have a single origin and cannot differ.
    pub code: String,
    /// The requesting `client_id` — a URL, rendered **verbatim**. Nothing
    /// derives an origin, a display name or a fetch target from it: that string
    /// is parsed exactly once, by the component that dials it.
    pub client_id: String,
    /// The client's name from its *resolved* metadata document, when it
    /// published one. `None` renders as the `client_id` alone — never a
    /// self-asserted string this side accepted unchecked.
    pub client_name: Option<String>,
    /// What the client is asking for, one human-readable line per requested
    /// scope, in the row's order.
    ///
    /// Composed by the machine from `fauna_bridge_atproto::authz::describe_scope`
    /// — the **one** owner of scope wording, shared with the browser consent
    /// page. The two surfaces sit side by side during the ceremony, so a wording
    /// divergence between them is precisely the "is this the same request?"
    /// doubt the binding code exists to remove. Never hand-write a second
    /// wording here or in a client.
    ///
    /// This is the **effective** list — a permission set's members are already
    /// in it, and the bare `include:` scope is not. [`sets`](Self::sets) says
    /// which of these lines the user is being asked to trust *as a set*; it
    /// never replaces them.
    pub scope_descriptions: Vec<String>,
    /// The permission sets this request named, frozen as PAR expanded them
    /// (`atproto-pds-full.md:334`). Empty for the ordinary request that named
    /// none, which is every request an app built before PS-b will ever see.
    pub sets: Vec<ConsentSetRow>,
    /// What approving this card ends besides granting it, rendered after the
    /// scope lines: a consent attesting a key other than the one the roster
    /// names for the same client ends the old key's third-party grants
    /// (`third-party.md` § The principal model → *Key replacement*). Set by
    /// the connected-apps machine, the one composer that reads the roster;
    /// `None` from [`consent_card_row`](crate::consent_card_row) alone.
    #[serde(default)]
    #[cfg_attr(feature = "uniffi", uniffi(default = None))]
    pub ends: Option<LocalizedText>,
}

/// One permission set on the consent card — what the user is being asked to
/// trust when a client names a set instead of listing scopes.
///
/// **Every field is composed in the shared machine, and that is the whole
/// design** (`atproto-pds-full.md:334`): `title` and `details` are
/// attacker-authored (a Lexicon record published by whatever DID the NSID's
/// authority names), so they cross the same control-strip fence `client_name`
/// does, in the same pass — a per-app painter fixing it six more times is the
/// shape that rule exists to prevent, and a *structural* per-app fix is not
/// sufficient anyway, since a title carrying fifty newlines still paints fifty
/// rows.
///
/// **The set-level description never stands in for the expansion.** Every
/// member crosses to all 7 apps as its own line; collapsing them behind a
/// disclosure is per-form-factor *rendering* of data that always crosses, never
/// a decision about what to send. A user approving "Calendar sync" is entitled
/// to read what that means without asking a second surface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ConsentSetRow {
    /// The set's NSID, rendered **verbatim** — the identity, exactly as
    /// `client_id` is for the client. Nothing derives a display name or an
    /// authority from it; it is the string the resolution chain was pointed at,
    /// and it is what a user checking a set against its publisher compares.
    pub nsid: String,
    /// The set's declared title, **control-character-stripped here**. `None`
    /// when the document declared none — which renders as the NSID alone,
    /// never as an invented label.
    pub title: Option<String>,
    /// The set's declared description, stripped in the same pass. Advisory
    /// prose from the set's author; see the type docs on why it cannot stand in
    /// for [`member_descriptions`](Self::member_descriptions).
    pub details: Option<String>,
    /// The expansion, one human-readable line per member, through the same
    /// `describe_scope` owner the flat list and the browser page use. These
    /// lines also appear in [`ConsentCardRow::scope_descriptions`] — a set
    /// contributes ordinary granular scopes, and the grant is made of those.
    pub member_descriptions: Vec<String>,
}

/// The hosted identity summary the hosted panel renders
/// (`atproto-hosted-handle` + method + active/deactivated state). The raw DID
/// string is never shown (`docs/goal/ui/atproto.md` § Layout & flow), so it is
/// deliberately absent here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct IdentitySummaryRow {
    /// The derived ATProto handle (`alice.example.com`); empty when the
    /// handle does not derive.
    pub handle: String,
    /// `"plc"` or `"web"` — a fact once minted, displayed not chosen.
    pub method: String,
    /// `"pending"` (mint owed), `"active"`, or `"deactivated"` (rendered as
    /// such so the user sees what re-enabling would restore).
    pub status: String,
}

/// The consume-side link summary — enough for the Linked panel header and the
/// transition card's unlink line. The full link surface (settings, follows)
/// keeps its existing owner (`fauna.bridges.*`); the machine composes, it
/// does not re-own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct LinkSummaryRow {
    /// The linked external identity's display string ("@alice.bsky.social").
    pub display: String,
}

/// The staged transition card (`atproto-depth-confirm-card`). Present exactly
/// while a level change awaits the user's confirm; the level NEVER changes on
/// selection alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct TransitionCardModel {
    /// The wire spelling of the level the confirm would move to.
    pub target_level: String,
    /// The card copy, one localized line per effect, composed from the shared
    /// [`TransitionPlan`](fauna_protocol::atproto::TransitionPlan) — the
    /// single description nest executes and this card describes. Clients
    /// render these verbatim and never build their own transition table.
    pub lines: Vec<LocalizedText>,
    /// Render the `atproto-history-backfill` checkbox on the card (the second
    /// explicit opt-in; only a minting transition offers it).
    pub show_history_backfill: bool,
    /// The confirm is in flight (a PLC mint round-trips): the card shows
    /// progress and the confirm control disables.
    pub in_progress: bool,
}

/// The recovery-fork contest card (`ui/atproto.md` § Element IDs,
/// `atproto-contest-card`) — what the page renders at the top when a standing
/// custody violation names a box-authored operation on this identity.
///
/// **Composed entirely from client-side evidence** (the client's own read of
/// the public PLC directory), never from nest testimony — so the remedy stays
/// reachable exactly when a hostile box is denying the identity, which is the
/// only time it matters (the audit-floor rule).
///
/// The row deliberately carries no contested-op CID: the confirm gesture takes
/// no arguments and acts on the plan the machine just re-derived, so a client
/// cannot name a *different* operation than the one the card described. Same
/// reasoning as [`ConsentCardRow`]'s deliberate absences.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContestCardRow {
    /// The `state` attribute of `atproto-contest-card`:
    /// `"contestable"` | `"window-closed"` | `"not-contestable"`.
    ///
    /// The three states are assertable rather than inferred from which
    /// controls render, because "you can still fix this", "you are too late"
    /// and "this can never be fixed" are different things to tell a user whose
    /// identity is under attack.
    pub state: String,
    /// `atproto-contest-detail` — what was done and what undoing it would do,
    /// composed by the machine so seven apps cannot word it seven ways.
    pub detail: LocalizedText,
    /// `atproto-contest-deadline` — the advisory countdown to the 72 h
    /// window's close. `None` when the directory published no parseable
    /// timestamp (the contest is still offered — an unknown deadline is not a
    /// closed window) or when the state is already terminal.
    pub deadline: Option<LocalizedText>,
    /// Render `atproto-contest`: only in the `contestable` state. A dead button
    /// on a hopeless state is exactly what decision 2 rules out.
    pub show_contest: bool,
}

/// The contest ceremony's confirm card (`atproto-contest-confirm-card`).
/// Present exactly while the user has opened the ceremony and not yet cancelled
/// it; nothing is signed and no consent is recorded until
/// [`AtprotoSettingsMachine::request_contest`](crate::AtprotoSettingsMachine::request_contest).
///
/// **The open/cancel state lives on the machine, not in a page.** It is the last
/// surface a user sees before a signature that rewrites who controls their
/// identity, and there are seven shells: a per-app `bool` would be seven chances
/// to render a confirm over a violation that cannot be fought, or to leave the
/// button live through a second press. Same argument, and the same shape, as
/// [`TransitionCardModel`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ContestConfirmCardModel {
    /// The card copy, composed machine-side: what is being undone, what this
    /// device signs, what the directory rules on, and the advisory deadline
    /// (`ui/atproto.md` § User actions requires the first, second and last).
    /// Rendered verbatim — a shell derives nothing and words nothing itself.
    pub lines: Vec<LocalizedText>,
    /// The submit is in flight: the confirm control disables so a second press
    /// cannot sign a second time.
    pub in_progress: bool,
}

/// The "Delete my Bluesky presence" confirm card (`atproto-delete-confirm-card`
/// — deliberately its **own** card, never the depth selector's transition card,
/// `ui/atproto.md` § User actions row 4).
///
/// Present exactly while the ceremony is open; nothing is destroyed on opening
/// it. The copy is composed machine-side so seven apps render one ceremony
/// rather than seven — and, more particularly, so no app can word the promise
/// itself: the sweep is *"still reversible in identity terms"*
/// (`atproto-pds-bridge.md` § Disable & revocation layer 2) and it cannot recall
/// what the public network already copied, and a card that overstated either
/// would be the client re-deriving semantics it does not own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DeleteConfirmCardModel {
    /// The card copy, composed by `compose_delete_confirm_lines`: what the
    /// sweep destroys, that the identity survives (named), the honest caveat,
    /// and where the level lands. Rendered verbatim — a shell derives nothing.
    pub lines: Vec<LocalizedText>,
    /// The confirm is in flight: the control disables so a second press cannot
    /// send a second sweep.
    pub in_progress: bool,
    /// The "also permanently retire this identity" opt-in
    /// (`atproto-delete-tombstone`, S5 slice 5b). Always present while the card
    /// is open — greyed with its reason where the identity cannot be retired,
    /// never hidden, so the stronger act is discoverable and never a control
    /// that errors on press.
    pub retire_identity: RetireIdentityOptIn,
}

/// The delete ceremony's terminal opt-in (`atproto-delete-tombstone`): publish
/// a PLC tombstone once the sweep finishes, retiring the DID for good
/// (`atproto-pds-bridge.md` § Disable & revocation layer 2).
///
/// Offered **only here**, only unticked, and only for a published did:plc
/// identity (`ui/atproto.md` § Don't do these). Ticking it also swaps the
/// card's "your identity is kept" line for the terminal one, so the card never
/// promises the identity survives while the act that destroys it is selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct RetireIdentityOptIn {
    /// The checkbox is live. `false` renders it greyed with
    /// [`Self::unavailable_reason`].
    pub available: bool,
    /// The user ticked it on this open card. Starts `false` on every opening.
    pub selected: bool,
    /// Why the identity cannot be retired (did:web has no operation log; a
    /// did:plc not yet published has nothing to tombstone). `None` exactly
    /// when [`Self::available`].
    pub unavailable_reason: Option<LocalizedText>,
}

/// The whole renderable ATProto settings surface in one record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AtprotoSettingsSnapshot {
    // ── The depth selector (`ui/atproto.md` § Layout & flow) ────────────
    /// The current level's wire spelling (`"off"` | `"linked"` |
    /// `"hosted_visible"` | `"hosted_full"`). Defaults to `"off"` before the
    /// first fetch — the nest column default, same convention as
    /// `external_apps_enabled` below.
    pub level: String,
    /// The real-domain gate verdict, nest-computed: `false` greys the two
    /// hosted options (never hides them — `hosted_gate_reason` says why).
    pub hosted_allowed: bool,
    /// The one-line reason shown on the greyed hosted options; `None` when
    /// the gate passes.
    pub hosted_gate_reason: Option<LocalizedText>,
    /// The would-be ATProto handle (`alice.example.com`) for the "your handle
    /// is @you.yourdomain either way" line; empty when unknown.
    pub handle_preview: String,
    /// The hosted identity summary, present at any status — a deactivated
    /// identity stays visible so the user sees what re-enabling restores.
    pub identity: Option<IdentitySummaryRow>,
    /// The consume-side link summary; `None` when no external account is
    /// linked.
    pub link: Option<LinkSummaryRow>,
    /// The staged transition card; `None` when no level change is pending.
    pub pending_transition: Option<TransitionCardModel>,
    /// The DID-method radio state (`"plc"` default, `"web"` opt-in). The
    /// radio renders only while [`Self::show_did_method_radio`].
    pub did_method: String,
    /// Render the DID-method radio: only before any identity exists — after
    /// mint the method is a fact, displayed not chosen.
    pub show_did_method_radio: bool,
    /// The `atproto-history-backfill` checkbox state (rides the transition
    /// card of a minting transition).
    pub history_backfill: bool,
    /// Render `atproto-delete-presence`: whenever a hosted identity exists,
    /// active or deactivated, so the stronger action stays reachable after a
    /// step-down — and **not** once the presence is already destroyed
    /// (`deleted` / `tombstoned`), where the gesture's only outcome is a no-op
    /// and the control would be the dead button `open_contest_confirm` refuses
    /// to paint. `ui/atproto.md` § User actions row 4 enumerates the two
    /// statuses that offer it.
    pub show_delete_presence: bool,
    /// The delete ceremony's confirm card, open only on the user's explicit
    /// `atproto-delete-presence` gesture and never over a presence there is
    /// nothing left to delete.
    pub delete_confirm: Option<DeleteConfirmCardModel>,

    // ── The F1 login-plane surface ──────────────────────────────────────
    /// `atproto-app-credentials-list` rows, in the nest's order.
    pub credentials: Vec<AppCredentialRow>,
    /// `atproto-connected-apps-list` rows — live sessions only. (The OAuth
    /// *grant* rows that share that group arrive with F4.)
    pub sessions: Vec<AtprotoSessionRow>,
    /// The `atproto-external-apps-enable` toggle. Default ON; OFF suspends the
    /// whole external-app plane non-destructively — rows stay listed and
    /// individually revocable, which is why they are still rendered when this
    /// is `false`.
    pub external_apps_enabled: bool,
    /// The D10 authoring-delegation row — what authorizes an external ATProto
    /// app to post as this account. `None` when no delegation is provisioned,
    /// which is the state in which external writes are refused with the D6
    /// *fauna-surface* sub-type naming this very control.
    pub delegation: Option<DelegationRow>,
    /// Pending OAuth consent requests this account may answer
    /// (`atproto-consent-card`), oldest first — the nest's order, kept.
    ///
    /// Includes **unassigned** requests: a PAR that carried no `login_hint`
    /// names no account, so it is listed to every account on the nest and
    /// claimed by whoever answers it. Empty is the overwhelmingly common state;
    /// a row appears only while an external app is mid-ceremony.
    pub consents: Vec<ConsentCardRow>,
    /// The recovery-fork contest card, rendered at the TOP of the page —
    /// above the depth selector — whenever a standing custody violation names
    /// a box-authored op on this identity. `None` in every ordinary state; an
    /// identity under active attack outranks every settings row below it.
    pub contest: Option<ContestCardRow>,
    /// The contest ceremony's confirm card, open only on the user's explicit
    /// gesture and never without a [`Self::contest`] above it to confirm.
    pub contest_confirm: Option<ContestConfirmCardModel>,
    /// The page-level `error-message` (last gesture / refresh failure),
    /// localized client-side; `None` when clear.
    pub error: Option<LocalizedText>,
}

/// The delegation status row (`atproto-pds-full.md` § D10 → Mint ceremony:
/// "the atproto page's hosted level shows the delegation").
///
/// **Secret-free, like every other row on this snapshot** — a snapshot is
/// cloned into every observing view on every tick. `device_key` is the sub-key's
/// *public* half; the secret never leaves the nest process at all.
///
/// Every field here was recovered from the stored cert and **verified under the
/// account's own identity key** (`parse_delegation_cert`), so this row states
/// what the user actually signed rather than what the nest reported.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct DelegationRow {
    /// The authorized sub-key's public half, hex — the page shows an
    /// abbreviation, the same way `nests-item-nest-id` abbreviates.
    pub device_key_hex: String,
    /// The granted capabilities in cert order, as their wire spellings
    /// (`"Post"`, `"UpdateProfile"`).
    pub capabilities: Vec<String>,
    /// When the user authorized it — epoch **microseconds** (`Timestamp`'s
    /// unit, not the milliseconds the `fauna.bridges.atproto.*` credential and
    /// session rows use; these come from the signed cert, not the wire).
    pub authorized_at_micros: u64,
    /// When it lapses; `None` for a cert carrying no expiry.
    pub expires_at_micros: Option<u64>,
    /// Whether the delegation is live, close to lapsing, or lapsed — so the
    /// page can warn *before* external apps stop working, and read a lapse as
    /// "re-authorize here" rather than as silent feature loss.
    ///
    /// Carried as its wire spelling (`"active"` | `"expiring_soon"` |
    /// `"expired"` | `"never_expires"`), the same convention `level`,
    /// `did_method` and the session rows' `plane` already use on this
    /// snapshot — and the same reason D8's `plane` crosses its boundary as a
    /// string: an unrecognized value stays *representable*, so a shell built
    /// against an older spelling set degrades instead of failing to decode.
    pub liveness: String,
    /// Epoch-**milliseconds** of the last external-app write that actually
    /// applied under this delegation; `None` = not used yet, rendered as the
    /// "not used yet" text.
    ///
    /// **ADVISORY ONLY — the page must present it as a hint, never as proof**
    /// (D10 § Audit). Every other field on this row is derived from the *signed
    /// cert*, verified client-side under the account's own identity key; this
    /// one is a bare nest assertion with nothing signing it. So it is not proof
    /// of use, and — the direction that actually matters — **not proof of
    /// non-use**: a compromised nest can under-report it at will, and a refused
    /// attempt never stamps it. The real audit surface is the delegated content
    /// itself (the `delegated-origin-badge` on the feed). Milliseconds here, not
    /// the microseconds the cert-derived timestamps above use, because this
    /// value comes from the wire rather than from the cert.
    pub last_used_at_millis: Option<i64>,
}

/// One granted capability's **wire spelling** in user voice, for
/// `atproto-delegation-scope`.
///
/// Shared rather than per-app because the input is a wire spelling and the
/// output is an i18n key: every app that renders the leaf would otherwise
/// re-derive the same two-arm map, and the fourth copy is where they start
/// disagreeing (priority #4).
///
/// An unrecognized capability degrades to its **wire form** rather than
/// vanishing — `LocalizedText::resolve` falls back to the key itself when no
/// string matches — so a newer nest granting a capability this build has never
/// heard of still shows the user that something was granted. Silently dropping
/// it would understate a grant, which is the one direction an audit surface
/// must never err in.
///
/// A **free function**, not only the [`DelegationRow::capability_labels`]
/// method below, because a shell reaching this machine across a *boundary*
/// (wasm today; UniFFI when the natives stop hand-rolling their Swift/Kotlin/C#
/// copies) receives the row as serialized fields and has no `DelegationRow` to
/// call a method on. Without this door the only way to paint the leaf over
/// there is a fourth hand-written map — the exact thing the sharing exists to
/// prevent.
pub fn delegation_capability_label(capability: &str) -> LocalizedText {
    LocalizedText::key(match capability {
        "Post" => "atproto_settings.delegation_capability_post",
        "UpdateProfile" => "atproto_settings.delegation_capability_update_profile",
        other => other,
    })
}

/// A [`DelegationRow::liveness`] wire spelling in user voice, for
/// `atproto-delegation-status`.
///
/// Same shared-map and cross-boundary reasoning as
/// [`delegation_capability_label`], and the same degrade-never-fail rule the
/// `liveness` field's own docs state: an unrecognized spelling from a newer
/// nest renders verbatim rather than blanking the row. ⚠ The **wire** spelling
/// — not this text — is what the e2e asserts, carried in each app's `state`
/// attr on the leaf, so a wording change never breaks a test and a test never
/// pins prose.
pub fn delegation_liveness_label(liveness: &str) -> LocalizedText {
    LocalizedText::key(match liveness {
        "active" => "atproto_settings.delegation_status_active",
        "expiring_soon" => "atproto_settings.delegation_status_expiring_soon",
        "expired" => "atproto_settings.delegation_status_expired",
        "never_expires" => "atproto_settings.delegation_status_never_expires",
        other => other,
    })
}

/// An [`IdentitySummaryRow::status`] wire spelling in user voice, for
/// `atproto-hosted-handle`.
///
/// The one reading of the identity status on every app: tui and linux call it
/// in-process, android/apple/windows over UniFFI, web over wasm — the same
/// shared-map and cross-boundary reasoning as [`delegation_liveness_label`].
/// An unrecognized status degrades to its **wire form** rather than blanking
/// the row (`LocalizedText::resolve` falls back to the key itself).
pub fn identity_status_label(status: &str) -> LocalizedText {
    LocalizedText::key(match status {
        "active" => "atproto_settings.identity_status_active",
        "pending" => "atproto_settings.identity_status_pending",
        "deactivated" => "atproto_settings.identity_status_deactivated",
        "deleted" => "atproto_settings.identity_status_deleted",
        "tombstoned" => "atproto_settings.identity_status_tombstoned",
        other => other,
    })
}

impl DelegationRow {
    /// The granted capabilities in **user voice**, one [`LocalizedText`] per
    /// capability in cert order — the page joins them into its own
    /// `atproto_settings.delegation_scope_prefix` line.
    ///
    /// The convenience form of [`delegation_capability_label`] for a shell that
    /// holds the row as Rust (tui, linux); the map itself lives in the free
    /// function so a cross-boundary shell reaches the same answer.
    pub fn capability_labels(&self) -> Vec<LocalizedText> {
        self.capabilities
            .iter()
            .map(|c| delegation_capability_label(c))
            .collect()
    }

    /// [`Self::liveness`] in user voice, for `atproto-delegation-status` — the
    /// convenience form of [`delegation_liveness_label`], same split.
    pub fn status_label(&self) -> LocalizedText {
        delegation_liveness_label(&self.liveness)
    }
}

/// Pre-fetch catalog defaults (the nest column defaults named on each field):
/// `level` `"off"`, `did_method` `"plc"`, `external_apps_enabled` ON, all
/// summaries/cards absent, the hosted gate closed **and carrying its reason**.
/// Exists so fixtures use struct-update syntax (`..Default::default()`) and
/// two branches growing this record merge cleanly instead of colliding on the
/// grown axis.
impl Default for AtprotoSettingsSnapshot {
    fn default() -> Self {
        Self {
            level: "off".to_string(),
            hosted_allowed: false,
            // NOT `None`. This field's contract is "`None` when the gate
            // passes" (see its doc above), and the pre-fetch gate does *not*
            // pass — it is closed as the safe default, before the nest has
            // been asked. Leaving it `None` here was the one construction path
            // that broke the invariant, and every app renders the two hosted
            // rungs off this record on first paint: they came up greyed with
            // no reason at all, which `ui/README.md` rule 5 forbids. The
            // machine's own `snapshot()` names the offending domain; pre-fetch
            // there is no domain to name, so this says what is being checked.
            hosted_gate_reason: Some(LocalizedText::key("atproto_settings.gate_reason_pending")),
            handle_preview: String::new(),
            identity: None,
            link: None,
            pending_transition: None,
            did_method: "plc".to_string(),
            // No identity exists pre-fetch, so the radio renders — mirrors
            // `AtprotoSettingsMachine::snapshot()`'s `identity.is_none()`.
            show_did_method_radio: true,
            history_backfill: false,
            show_delete_presence: false,
            delete_confirm: None,
            credentials: Vec::new(),
            sessions: Vec::new(),
            external_apps_enabled: true,
            delegation: None,
            consents: Vec::new(),
            contest: None,
            contest_confirm: None,
            error: None,
        }
    }
}

impl AtprotoSettingsSnapshot {
    /// Whether the page has fetched anything yet — a first paint renders the
    /// empty state rather than "no credentials" when this is `false` and no
    /// error is set.
    pub fn is_empty(&self) -> bool {
        self.credentials.is_empty() && self.sessions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn delegation(capabilities: &[&str], liveness: &str) -> DelegationRow {
        DelegationRow {
            device_key_hex: "ab".repeat(32),
            capabilities: capabilities.iter().map(|c| c.to_string()).collect(),
            authorized_at_micros: 1_700_000_000_000_000,
            expires_at_micros: None,
            liveness: liveness.to_string(),
            last_used_at_millis: None,
        }
    }

    /// Every capability wire spelling the cert vocabulary defines maps to a
    /// key — one shared answer, so the apps cannot drift on what a grant *says*
    /// it granted (priority #4). Stated per-variant rather than as a count, so
    /// adding a capability without its string fails here rather than shipping a
    /// row that shows the user a raw `CamelCase` token.
    #[test]
    fn every_known_capability_maps_to_a_string_key() {
        let labels = delegation(&["Post", "UpdateProfile"], "active").capability_labels();
        let keys: Vec<&str> = labels.iter().map(|l| l.key.as_str()).collect();
        assert_eq!(
            keys,
            vec![
                "atproto_settings.delegation_capability_post",
                "atproto_settings.delegation_capability_update_profile",
            ],
            "cert order is preserved and both known capabilities are localized"
        );
    }

    /// An unknown capability degrades to its **wire form** rather than
    /// vanishing. The direction matters: silently dropping it would *understate*
    /// a grant the user actually made, and an audit surface that under-reports
    /// what was granted is worse than one that shows an ugly token.
    #[test]
    fn an_unknown_capability_degrades_to_its_wire_form_rather_than_vanishing() {
        let labels =
            delegation(&["Post", "SomethingNewerNestsGrant"], "active").capability_labels();
        assert_eq!(
            labels.len(),
            2,
            "the unknown capability must still be shown"
        );
        assert_eq!(labels[1].key, "SomethingNewerNestsGrant");
        assert_eq!(
            labels[1].clone().resolve(|_: &str| None::<&str>),
            "SomethingNewerNestsGrant",
            "with no string table match the key itself is what the user sees"
        );
    }

    /// Every liveness wire spelling maps to a key, and an unrecognized one from
    /// a newer nest renders verbatim instead of blanking the row — the same
    /// degrade-never-fail rule the `liveness` field's own docs state.
    #[test]
    fn every_known_liveness_maps_to_a_string_key_and_unknown_degrades() {
        for (wire, key) in [
            ("active", "atproto_settings.delegation_status_active"),
            (
                "expiring_soon",
                "atproto_settings.delegation_status_expiring_soon",
            ),
            ("expired", "atproto_settings.delegation_status_expired"),
            (
                "never_expires",
                "atproto_settings.delegation_status_never_expires",
            ),
        ] {
            assert_eq!(
                delegation(&["Post"], wire).status_label().key,
                key,
                "liveness {wire} must localize"
            );
        }
        assert_eq!(
            delegation(&["Post"], "quarantined_by_a_newer_nest")
                .status_label()
                .key,
            "quarantined_by_a_newer_nest",
            "an unrecognized spelling stays representable — never a blank row"
        );
    }

    /// Every identity status wire spelling maps to its i18n key, and an
    /// unrecognized one paints the raw wire word.
    #[test]
    fn every_known_identity_status_maps_to_a_key_and_unknown_degrades() {
        for wire in ["active", "pending", "deactivated", "deleted", "tombstoned"] {
            let key = format!("atproto_settings.identity_status_{wire}");
            assert_eq!(identity_status_label(wire).key, key, "status {wire}");
            assert!(
                fauna_i18n::strings::lookup(&key).is_some(),
                "{key} must be a real string"
            );
        }
        assert_eq!(identity_status_label("something_new").key, "something_new");
    }

    /// The `hosted_gate_reason` contract, pinned at the one construction path
    /// that used to break it. Every app greys the two hosted rungs off
    /// `hosted_allowed` and prints the reason beside them, so a closed gate
    /// with no reason is a screen full of DIM controls that explain nothing
    /// (`docs/goal/ui/README.md` § Copy comprehensibility, rule 5).
    ///
    /// Stated as the biconditional rather than "the default has a reason", so
    /// it also catches a future default that opens the gate but keeps a stale
    /// line.
    #[test]
    fn the_prefetch_default_closes_the_hosted_gate_and_says_why() {
        let snap = AtprotoSettingsSnapshot::default();
        assert_eq!(
            snap.hosted_allowed,
            snap.hosted_gate_reason.is_none(),
            "a reason must be present exactly when the gate is closed; got \
             hosted_allowed={} reason={:?}",
            snap.hosted_allowed,
            snap.hosted_gate_reason,
        );
        assert!(
            !snap.hosted_allowed,
            "pre-fetch the gate stays closed until the nest answers"
        );
    }

    /// The pre-fetch default survives **serde JSON** with its gate reason
    /// intact — the shape web actually parses.
    ///
    /// The UniFFI seam hands native shells the record itself, but the wasm seam
    /// hands web a JSON string (`atprotoSettingsPrefetchSnapshot`), and web then
    /// renders the gate reason off the parsed object. A serde attribute that
    /// skipped `None`-able fields, or renamed one, would leave web back where
    /// its deleted `PENDING_GATE_REASON` literal started — a closed gate with no
    /// reason — while every native shell stayed correct, so the Rust-record test
    /// above would not catch it.
    #[test]
    fn the_prefetch_default_keeps_its_gate_reason_through_json() {
        let json = serde_json::to_string(&AtprotoSettingsSnapshot::default())
            .expect("the pre-fetch default serializes");
        let back: serde_json::Value = serde_json::from_str(&json).expect("and parses back");

        assert_eq!(
            back["hosted_allowed"], false,
            "the gate is closed pre-fetch on the wire too"
        );
        assert_eq!(
            back["hosted_gate_reason"]["key"], "atproto_settings.gate_reason_pending",
            "and it carries the reason web renders (rule 5); got {json}"
        );
        // The four other deliberately non-zero defaults — the ones a
        // hand-rolled stand-in got wrong on android, twice.
        assert_eq!(back["level"], "off");
        assert_eq!(back["did_method"], "plc");
        assert_eq!(back["show_did_method_radio"], true);
        assert_eq!(back["external_apps_enabled"], true);
    }
}
