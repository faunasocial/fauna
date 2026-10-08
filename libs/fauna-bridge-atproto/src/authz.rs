//! D8 — the single per-request authorization decision point for the hosted
//! PDS (`docs/goal/behavior/atproto-pds-full.md` § F3 detail, the *Ratified
//! contract* block).
//!
//! One module decides every authenticated XRPC call, for **both** credential
//! planes: the app-credential plane (F1, live) and the OAuth plane (F4, whose
//! arm is defined here and consumed later). That is the whole point — F4 adds
//! a *caller*, never a second enforcement path.
//!
//! # Contract
//!
//! [`authorize`] is a **pure function of its inputs**: no I/O, no clock, no
//! network, wasm-clean like the rest of this crate's core. Everything the
//! decision needs is in [`AuthzInput`], which is pinned to fields the Go XRPC
//! frame already carries (`xrpc.Caller.Plane`, `Caller.Scope`, the route's
//! NSID and `EndpointClass`, and the resolved proxy target). Purity is what
//! makes the matrix table-driven-testable in Rust and keeps the Go side to
//! assembly + enforcement.
//!
//! # Closed world
//!
//! The matrix is **default-deny**. An input combination this module does not
//! recognize — an unknown plane, an unparseable scope, an unclassified `lxm`
//! on a guarded surface, an `endpoint_class` the frame grew and this module
//! has not learned — **denies**. Go never interprets a verdict and never
//! falls through to allow.
//!
//! # Deny sub-typing (D6, `atproto-pds-full.md` § Problem 4)
//!
//! Refusals are sub-typed and the distinction is load-bearing; the XRPC error
//! *name* carries it, because that is what an XRPC error name is for:
//!
//! | Name | Sub-type | Meaning |
//! |---|---|---|
//! | `AuthenticationRequired` | — | Account-disabling or unrecognized input. **Uniform**: identical to a bad token, so nothing enumerates. |
//! | `InvalidToken` | policy-refused / fauna-surface | The presented credential's grant does not cover this call. The message says where the capability *does* live. |
//! | `MethodNotImplemented` | **deferred** | Planned but unbuilt (account migration). The message says "not yet" — a later session must never harden this into policy. |

use serde::{Deserialize, Serialize};

// ── Plane names, verbatim as `xrpc.Caller.Plane` reports them ────────────────

/// App-credential plane (F1): `com.atproto.server.createSession` sessions.
pub const PLANE_APP_CREDENTIAL: &str = "app_credential";
/// OAuth plane (F4): DPoP-bound authorization-server grants.
pub const PLANE_OAUTH: &str = "oauth";

// ── Token scopes the app plane mints (`internal/atprotopds/token.go`) ────────

/// Ordinary app-credential scope — everything but the DM class.
pub const SCOPE_APP_PASS: &str = "com.atproto.appPass";
/// Privileged app-credential scope — adds the DM class. Minted only when the
/// credential was created with `dm_allowed`.
pub const SCOPE_APP_PASS_PRIVILEGED: &str = "com.atproto.appPassPrivileged";

// ── OAuth scopes (F4) ────────────────────────────────────────────────────────

/// The spec-mandatory **base** scope every ATProto authorization request must
/// carry (`atproto.com/specs/oauth`; § Ecosystem reality item 2's grammar).
///
/// It grants *basic account identity* and nothing else — see
/// [`granular_scope_permits`] for what that resolves to here, and why the
/// answer is deliberately the narrowest one that still makes an
/// `atproto`-only grant usable.
pub const SCOPE_ATPROTO_BASE: &str = "atproto";

// ── The OIDC family (TP6) ────────────────────────────────────────────────────
//
// `docs/goal/behavior/authorization-server.md` § Scope grammar — three
// families under one predicate; the OIDC family is FIXED: these three strings
// and no grammar. They live here, beside the ATProto family, because
// [`scope_grants_something`] is the one predicate both PAR and the discovery
// document consult, and it must know every family it answers for.

/// OIDC's own scope: its presence is what makes a grant an OpenID Connect
/// sign-in — an `id_token` in the token reply and `/oauth/userinfo` answering
/// under the grant's access token (`authorization-server.md` § OIDC (TP6)).
pub const SCOPE_OPENID: &str = "openid";
/// The `profile` claims — here `preferred_username`, the account's handle.
pub const SCOPE_PROFILE: &str = "profile";
/// The `email` claims — the account's canonical mailbox address, when it has
/// one.
pub const SCOPE_EMAIL: &str = "email";

/// The whole OIDC family, in the order the discovery document lists it.
///
/// **Not part of [`GRANTABLE_SCOPES`], deliberately.** That list is also the
/// PDS's `scopes_supported` in the protected-resource document, and the PDS
/// honours none of these: they are the *issuer's* scopes, granting at the
/// issuer's own surfaces. Only the authorization server's document advertises
/// them (`crate::oauth_metadata::advertised_scopes`).
pub const OIDC_SCOPES: &[&str] = &[SCOPE_OPENID, SCOPE_PROFILE, SCOPE_EMAIL];

/// Is `scope` one of the fixed OIDC family?
#[must_use]
pub fn is_oidc_scope(scope: &str) -> bool {
    OIDC_SCOPES.contains(&scope)
}

/// Does this scope set break the ATProto family's entry rule — some
/// ATProto-family scope, and no base [`SCOPE_ATPROTO_BASE`] beside it?
///
/// The base scope is the ATProto **family's** requirement, not the server's
/// (`authorization-server.md` § Scope grammar): a request naming only the OIDC
/// and Fauna families names no ATProto scope and needs no base. One predicate for the three places the
/// rule is checked — a client's declared scope (fetched and loopback) and a
/// PAR's requested scope — so they cannot disagree about which client may
/// ask for what.
#[must_use]
pub fn lacks_atproto_base(scopes: &[String]) -> bool {
    scopes.iter().any(|s| is_atproto_family_scope(s))
        && !scopes.iter().any(|s| s == SCOPE_ATPROTO_BASE)
}

// ── The reader set ───────────────────────────────────────────────────────────
//
// `docs/goal/behavior/authorization-server.md` § The issuer → *The audience is
// the set of readers*. The family a scope belongs to decides which resource
// server it can be exercised at, and so who an access token's `aud` names.

pub use crate::fauna_scope::FAUNA_SCOPE_PREFIX;

/// Is `scope` a Fauna-family scope — one exercised at the nest itself?
///
/// Membership, by prefix — not grantability: which of the family's strings
/// an arm grants is [`crate::fauna_scope::arm_of`]'s question. The audience
/// derivation needs only to know which reader a scope belongs to.
#[must_use]
pub fn is_fauna_scope(scope: &str) -> bool {
    scope.starts_with(FAUNA_SCOPE_PREFIX)
}

/// Is `scope` an ATProto-family scope — one exercised at the PDS?
///
/// The ATProto family has an open grammar, so it is defined as what the two
/// closed families are not.
#[must_use]
pub fn is_atproto_family_scope(scope: &str) -> bool {
    !is_oidc_scope(scope) && !is_fauna_scope(scope)
}

/// The `aud` of an access token minted for `scopes`: each resource server the
/// grant can be exercised at.
///
/// The PDS service DID when the grant holds an ATProto-family scope, the nest
/// issuer identifier when it holds an OIDC or Fauna-family scope — in that
/// order, whatever order the scopes came in, so one grant has one spelling.
/// One reader goes on the wire as a string and two as an array; that encoding
/// is the token's, not this function's.
///
/// **Only the mint calls this.** A reader never re-derives the set from the
/// token's scopes: it requires its own identifier to be a member and ignores
/// the rest.
///
/// ⚠ The audience is not the reach check. It keeps a token consented for one
/// server from being presented at another; what the token may do there is the
/// scope check's.
#[must_use]
pub fn access_token_audience(
    scopes: &[String],
    pds_service_did: &str,
    issuer: &str,
) -> Vec<String> {
    let mut readers = Vec::with_capacity(2);
    if scopes.iter().any(|s| is_atproto_family_scope(s)) {
        readers.push(pds_service_did.to_string());
    }
    if scopes.iter().any(|s| is_oidc_scope(s) || is_fauna_scope(s)) {
        readers.push(issuer.to_string());
    }
    readers
}

/// The scopes this authorization server advertises as grantable — the
/// `scopes_supported` member of both discovery documents
/// (`oauth_metadata`).
///
/// **The list lives here, beside the matrix that honours it, because its one
/// binding property is that D8 can actually grant something under every
/// entry.** An advertised scope the matrix denies everywhere is a document
/// that lies: a client requests it, the consent screen shows it, the grant
/// records it, and every call under it still denies. Pinned executably by
/// `every_advertised_scope_grants_something` — which walks this list through
/// [`authorize`] rather than re-stating what each ought to permit.
///
/// **Deliberately absent, each because the matrix grants nothing under it:**
/// `transition:email` (Fauna has no email account model — see
/// [`authorize_oauth`]), and `account:`/`identity:` (parsed but empty —
/// account management and handle authority live in Fauna, D6). A
/// collection-narrowed `repo:<collection>` is absent for a different reason:
/// it is a *deferred* gap, not an empty one — the collection lives in the
/// request body and the pinned input set does not carry it yet — so
/// advertising it would promise a narrowing this server would silently widen
/// by denying.
///
/// The parameterized families (`repo:`, `blob:`, `rpc:`) are advertised by
/// their **wildcard exemplar**: the grammar is open, so an exhaustive
/// enumeration does not exist, and the exemplar is both the honest maximum
/// and a real grantable value. A client narrows from it.
///
/// **`include:` — permission sets — is deliberately UNADVERTISED, and that is
/// an open question rather than a settled no** (`atproto-pds-full.md:335`; the
/// PS-b build, 2026-08-03). The exemplar rule is what excludes it: `include:*`
/// is not a real grantable value — the grammar takes an NSID, `*` fails NSID
/// validation before any resolution, and a wildcard could not be granted even
/// in principle, since what a set permits is decided by *its publisher's*
/// document. So advertising it would break the one binding property this list
/// has, which `every_advertised_scope_grants_something` enforces. This server
/// therefore *serves* `include:` scopes at PAR while its discovery documents
/// stay silent about them, and whether a real client discovers the support
/// anyway (or needs some other signal) is what F5's reference-client run is
/// there to observe. Pinned below so the silence is a recorded decision rather
/// than an oversight someone later "fixes" into a lying document.
pub const GRANTABLE_SCOPES: &[&str] = &[
    SCOPE_ATPROTO_BASE,
    "transition:generic",
    "transition:chat.bsky",
    "repo:*",
    "blob:*/*",
    "rpc:*?aud=*",
];

// ── Endpoint classes, verbatim as `xrpc.EndpointClass` names them ────────────

/// The credential-guessing surface (`createSession`, token endpoints).
pub const ENDPOINT_CLASS_AUTH: &str = "auth";
/// The anonymous mirror read surface.
pub const ENDPOINT_CLASS_PUBLIC_READ: &str = "public_read";
/// Authenticated non-write calls (`getSession`, proxying).
pub const ENDPOINT_CLASS_AUTHED: &str = "authed";
/// Authenticated repo *writes* (`com.atproto.repo.{createRecord,putRecord,
/// deleteRecord,applyWrites}`) — its own bucket because a write costs a funnel
/// commit plus a nest round-trip, so it carries a tighter per-IP window than the
/// authenticated read/proxy traffic `authed` covers (`atproto-pds-full.md`
/// § Wire & process topology names authed writes as their own starting
/// constant).
pub const ENDPOINT_CLASS_WRITE: &str = "write";
/// Blob uploads (`com.atproto.repo.uploadBlob`) — its own bucket rather than
/// `write`'s for two reasons (F2.4 slice 1). A record write's cost is a funnel
/// commit plus a nest round-trip over a few KB, while a blob body runs to the
/// per-blob ceiling, so sharing `write`'s window would let one IP push
/// ceiling×window bytes a minute — a bandwidth-and-storage cost a record write
/// cannot express. And an image post is `uploadBlob` *then* `createRecord`, so
/// one shared bucket would silently halve the effective post rate for exactly
/// the callers who upload media.
pub const ENDPOINT_CLASS_BLOB: &str = "blob";

// ── Ecosystem service DIDs (hard-coded constants — never config) ─────────────

/// The AppView `app.bsky.*` reads default to when no `atproto-proxy` header is
/// present (reference-PDS server-side convention, not XRPC spec —
/// `atproto-pds-full.md` § Ecosystem reality).
pub const APPVIEW_SERVICE_DID: &str = "did:web:api.bsky.app#bsky_appview";
/// The chat (DM) service. Reaching it requires the privileged scope.
pub const CHAT_SERVICE_DID: &str = "did:web:api.bsky.chat#bsky_chat";

/// [`APPVIEW_SERVICE_DID`] for the Go bridge — uniffi cannot export a bare
/// `const`, and a hard-coded Go copy could drift, at which point a headerless
/// read dials one service while D8 authorized against another.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn appview_service_did() -> String {
    APPVIEW_SERVICE_DID.to_string()
}

// ── XRPC error names (the deny sub-type — see the module docs) ───────────────

/// Uniform refusal: identical to a bad token, no enumeration signal.
pub const ERR_AUTH_REQUIRED: &str = "AuthenticationRequired";
/// The credential's grant does not cover this call.
pub const ERR_INVALID_TOKEN: &str = "InvalidToken";
/// Deferred — planned but unbuilt. Never policy.
pub const ERR_NOT_IMPLEMENTED: &str = "MethodNotImplemented";

/// Everything the authorization decision may consider.
///
/// Every field is something the Go frame already has — no new plumbing. The
/// string-typed fields (`plane`, `endpoint_class`) are strings *on purpose*:
/// an unrecognized value must be representable so the closed-world matrix can
/// deny it. A Rust enum here would make "unknown plane denies" unexpressible
/// from Go, and therefore dead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AuthzInput {
    /// `xrpc.Caller.Plane` verbatim: [`PLANE_APP_CREDENTIAL`] or [`PLANE_OAUTH`].
    pub plane: String,
    /// The scopes the presented credential carries.
    ///
    /// App plane: exactly one — the access token's `scope` claim. OAuth
    /// plane: the grant's granular scopes, with every `include:<NSID>`
    /// permission set **already expanded into its member scopes by the
    /// caller**. Expansion is frozen at the ceremony, so what arrives here is
    /// the grant row's own frozen list and this path never resolves anything.
    ///
    /// The split behind that (ratified 2026-08-03, § F4 detail's *Permission
    /// sets*): Go performs the resolution *fetch* — DNS, DID and the guarded,
    /// authenticated record read — and [`crate::permission_set`] performs the
    /// *expansion*, since grammar and document interpretation are exactly what
    /// belongs in a pure module. Either way this matrix is untouched: members
    /// arrive as ordinary granular scopes and [`no_granular_scope_reaches`]
    /// binds them by construction.
    pub scopes: Vec<String>,
    /// The account's external-apps kill-switch, as the bridge last learned it
    /// (`fauna.bridges.atproto.sessions_changed`). `false` suspends the
    /// account's entire external-app plane.
    pub external_apps_enabled: bool,
    /// The method being authorized: the route's NSID.
    ///
    /// `com.atproto.server.getServiceAuth` is checked **twice** — once with
    /// `lxm` = the route NSID (may this credential mint at all?) and once with
    /// `lxm`/`aud` = the *requested* method and audience (may it mint for
    /// that?). Two checks, one decision function; that is what keeps
    /// migration-oriented minting deferred-refused without a second code path.
    pub lxm: String,
    /// The resolved proxy-target service DID — present only on proxied calls
    /// and on the second `getServiceAuth` check. May carry a `#fragment`.
    pub aud: Option<String>,
    /// The route's declared `xrpc.EndpointClass`.
    pub endpoint_class: String,
}

/// The decision. `Deny` carries the XRPC error name (which encodes the D6
/// sub-type) and the human message; Go maps the name to an HTTP status through
/// a mechanical table and emits it verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum AuthzVerdict {
    Allow,
    Deny { xrpc_error: String, message: String },
}

impl AuthzVerdict {
    fn deny(xrpc_error: &str, message: &str) -> Self {
        AuthzVerdict::Deny {
            xrpc_error: xrpc_error.to_string(),
            message: message.to_string(),
        }
    }

    /// The uniform refusal — a disabled account, an unrecognized plane and a
    /// bad token must be indistinguishable from outside.
    fn uniform() -> Self {
        Self::deny(ERR_AUTH_REQUIRED, "authentication required")
    }

    /// True when this verdict permits the call.
    pub fn is_allow(&self) -> bool {
        matches!(self, AuthzVerdict::Allow)
    }
}

/// What kind of operation an `lxm` names. Module-internal: the classification
/// *is* the matrix, and exposing it would invite Go to branch on it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LexiconClass {
    /// Repo + mirror reads, and the identity read subset.
    Read,
    /// Repo mutations.
    Write,
    /// Blob upload — a class of its own because the OAuth grammar gates it
    /// separately (`blob:` scopes, not `repo:`).
    Blob,
    /// Basic account identity: who am I, and what is this server. The class
    /// the base `atproto` scope grants.
    SessionIdentity,
    /// Session lifecycle — minting, refreshing and ending a session.
    ///
    /// Split from [`LexiconClass::SessionIdentity`] deliberately: these three verbs are the **app-credential plane's own**
    /// lifecycle (D3 rung 1), and the OAuth plane has its own in
    /// `/oauth/token` and `/oauth/revoke`. An OAuth caller reaching them is a
    /// plane confusion, not a scope that is merely too wide — so no OAuth
    /// scope grants this class, not even a maximally granular one.
    ///
    /// Nothing reaches it today: all three routes register `Auth: Public` and
    /// verify the app plane's refresh token handler-side, so they never
    /// consult this matrix at all. The class exists because a *route
    /// registration* is what this matrix judges, and the day one of them is
    /// re-registered as authenticated, the base scope must not silently
    /// already permit it.
    SessionLifecycle,
    /// Private per-account preferences (served locally, D4 custody).
    Preferences,
    /// Direct messages — the `chat.bsky.*` surface.
    Dm,
    /// Proxied AppView traffic (`app.bsky.*` other than preferences).
    AppView,
    /// Service-auth minting.
    ServiceAuthMint,
    /// Account mutation, credential management, destructive account ops, and
    /// identity mutation — the class the app-credential grant excludes.
    AccountMutation,
    /// Account migration — **deferred**, never policy-refused: outbound
    /// migration is a guarantee (`atproto-pds-bridge.md` § Design horizon 3).
    Migration,
    /// Not known to this module.
    Unknown,
}

/// Authorize one request. Pure; see the module docs for the contract.
///
/// The Go atproto.pds bridge calls this from its `xrpc.AuthzHook` seat: it
/// assembles the input from the route + the verified caller and enforces the
/// verdict verbatim. **Go never interprets** — an unrecognized input denies
/// here, never falls through to allow there.
///
/// Public routes never reach this: `Auth: Public` in the route table *is* their
/// authorization, and a request with no authenticated plane has nothing for
/// this matrix to decide. `com.atproto.server.getServiceAuth` reaches it
/// *twice* — once for the route, once for the requested `lxm`/`aud`.
///
/// The export lives here rather than in `fauna-ffi` because the input/verdict
/// types do: uniffi-bindgen-go emits one Go package per uniffi namespace and
/// cannot resolve a type reference across two, so a function and the types it
/// takes must share a crate (the `fauna_mail::verify_inbound` precedent).
///
/// Takes the input by value because the FFI boundary hands over an owned
/// record; the internal helpers borrow it.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn authorize(input: AuthzInput) -> AuthzVerdict {
    let input = &input;
    // The kill-switch outranks every grant: OFF suspends the account's entire
    // external-app plane, privileged scopes included.
    if !input.external_apps_enabled {
        return AuthzVerdict::uniform();
    }
    // Closed world: a class the frame grew and this module has not learned is
    // a decision we cannot make, so we refuse to make it.
    if !matches!(
        input.endpoint_class.as_str(),
        ENDPOINT_CLASS_AUTH
            | ENDPOINT_CLASS_PUBLIC_READ
            | ENDPOINT_CLASS_AUTHED
            | ENDPOINT_CLASS_WRITE
            | ENDPOINT_CLASS_BLOB
    ) {
        return AuthzVerdict::uniform();
    }

    let class = classify(&input.lxm);
    let dm = class == LexiconClass::Dm
        || matches!(input.aud.as_deref(), Some(a) if same_did(a, CHAT_SERVICE_DID));

    match input.plane.as_str() {
        PLANE_APP_CREDENTIAL => authorize_app_credential(input, class, dm),
        PLANE_OAUTH => authorize_oauth(input, class, dm),
        _ => AuthzVerdict::uniform(),
    }
}

/// Can this authorization server grant **anything** under `scope`?
///
/// One predicate, two callers, and that is the whole point:
///
/// * [`GRANTABLE_SCOPES`] is pinned by walking each entry through it, so the
///   discovery documents cannot advertise a capability this matrix denies
///   everywhere (the property `every_advertised_scope_grants_something`
///   asserts).
/// * `crate::oauth_par` runs each **requested** scope through it, so a PAR
///   cannot be accepted for a scope the matrix denies everywhere either.
///
/// Before this existed, only the first half was checked, and the second was
/// the same lie moved one step later: a client requests
/// `repo:app.bsky.feed.post`, the consent screen shows it, the grant records
/// it, and every call under it still denies — with the user having approved
/// something that does nothing, and the client author with no diagnosis. The
/// two questions are one question, so they get one answer.
///
/// It asserts *that* something is granted, never *what*. Pinning per-scope
/// grants here would duplicate the matrix's own tests and turn every widening
/// into two edits.
///
/// # How the probes are chosen
///
/// A fixed representative `lxm` per lexicon class, **plus** — for an `rpc:`
/// scope — the `lxm` and `aud` the scope itself names, since a scope narrowed
/// to a specific method could never be covered by a fixed probe list. A
/// wildcard in either position falls back to a representative. That is not
/// circular: the question is "does the matrix allow the call this scope
/// describes", so deriving the probe from the scope is the question, and the
/// scopes that fail (`transition:email`, `account:`, `identity:`, a
/// collection-narrowed `repo:`) fail under *every* probe, derived or not.
///
/// # The OIDC family answers without a probe
///
/// `openid`, `profile` and `email` grant at the **issuer's** own surfaces — the
/// `id_token` and `/oauth/userinfo` — and never at the PDS, so no `lxm` could
/// witness them and none is asked: they answer `true` by membership
/// (`authorization-server.md` § Scope grammar — one predicate, three
/// families). What each one grants is pinned where it is granted, by the
/// nest's OIDC tests; this function only has to know the family exists.
///
/// # The Fauna family answers by arm
///
/// A `fauna:` scope grants something exactly when it matches a built arm of
/// the closed table ([`crate::fauna_scope::arm_of`]) — the family's predicate
/// made structural, since an arm exists only while a nest `ThirdParty` ceiling
/// kind names it. A Fauna-family string matching no arm grants nothing here
/// and is never probed against the PDS matrix, which it could not reach.
pub fn scope_grants_something(scope: &str) -> bool {
    if is_oidc_scope(scope) {
        return true;
    }
    if is_fauna_scope(scope) {
        return crate::fauna_scope::arm_of(scope).is_some();
    }
    // One representative lxm per lexicon class the matrix knows.
    const PROBES: &[(&str, Option<&str>)] = &[
        ("com.atproto.server.getSession", None),
        ("com.atproto.repo.getRecord", None),
        ("com.atproto.repo.createRecord", None),
        ("com.atproto.repo.uploadBlob", None),
        ("app.bsky.actor.getPreferences", None),
        ("app.bsky.feed.getTimeline", Some(APPVIEW_SERVICE_DID)),
        ("chat.bsky.convo.listConvos", Some(CHAT_SERVICE_DID)),
        (
            "com.atproto.server.getServiceAuth",
            Some(APPVIEW_SERVICE_DID),
        ),
    ];

    let mut probes: Vec<(String, Option<String>)> = PROBES
        .iter()
        .map(|(lxm, aud)| (lxm.to_string(), aud.map(str::to_string)))
        .collect();
    if let Some((lxm, aud)) = rpc_scope_probe(scope) {
        probes.push((lxm, Some(aud)));
    }

    probes.iter().any(|(lxm, aud)| {
        authorize(AuthzInput {
            plane: PLANE_OAUTH.to_string(),
            scopes: vec![scope.to_string()],
            external_apps_enabled: true,
            lxm: lxm.clone(),
            aud: aud.clone(),
            endpoint_class: ENDPOINT_CLASS_AUTHED.to_string(),
        })
        .is_allow()
    })
}

/// The `(lxm, aud)` call an `rpc:` scope describes, with a wildcard in either
/// position standing in for a representative value. `None` for any other
/// scope shape.
fn rpc_scope_probe(scope: &str) -> Option<(String, String)> {
    let rest = scope.strip_prefix("rpc:")?;
    let (lxm_pat, query) = rest.split_once('?')?;
    let aud_pat = query.strip_prefix("aud=")?;
    let lxm = if lxm_pat == "*" {
        "app.bsky.feed.getTimeline".to_string()
    } else {
        lxm_pat.to_string()
    };
    let aud = match percent_decode(aud_pat) {
        a if a == "*" => APPVIEW_SERVICE_DID.to_string(),
        a => a,
    };
    Some((lxm, aud))
}

/// A human-readable, one-line rendering of a scope — the string the consent
/// surfaces show a person deciding whether to approve.
///
/// One owner for BOTH consent surfaces — the browser page `/oauth/authorize`
/// serves (F4 slice 6b) and the in-app approval card (slice 6c) — because the
/// two render the same pending-consent row side by side, and a wording
/// divergence between them is exactly the "is this the same request?" doubt
/// the binding code exists to remove.
///
/// Deliberately **not** localized here: the strings are English, like the rest
/// of the wire-adjacent vocabulary this crate owns. If an app surface needs
/// localization later, these match arms are the enumeration to key i18n
/// entries from — the grammar knowledge stays in this one place either way.
///
/// The fallback is the scope string **verbatim** — an honest "no friendlier
/// name" rather than a guess. A scope reaching a consent surface has already
/// passed [`scope_grants_something`] at PAR, so the fallback arm is for
/// grammar this build predates, not for garbage.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn describe_scope(scope: String) -> String {
    // The OIDC family: three rows, each saying exactly which fact leaves —
    // `profile` and `email` are separate rows so a person can see that
    // approving one does not approve the other (`authorization-server.md`
    // § OIDC (TP6)).
    if scope == SCOPE_OPENID {
        return "Sign you in with your Fauna account (a stable account identifier)".to_string();
    }
    if scope == SCOPE_PROFILE {
        return "See your handle".to_string();
    }
    if scope == SCOPE_EMAIL {
        return "See your email address".to_string();
    }
    // The Fauna family: one row per built arm (per accepted qualifier, for an
    // arm taking one), owned by the grammar module.
    if let Some(row) = crate::fauna_scope::card_row(&scope) {
        return row.to_string();
    }
    if scope == SCOPE_ATPROTO_BASE {
        return "See your account identity (who you are on this server)".to_string();
    }
    if scope == "transition:generic" {
        return "Full account access, except direct messages (the standard app-password grant)"
            .to_string();
    }
    if scope == "transition:chat.bsky" {
        return "Access your direct messages (the privileged app-password grant)".to_string();
    }
    if let Some(collection) = scope.strip_prefix("repo:") {
        return if collection == "*" {
            "Create, edit and delete records of any type in your public repository".to_string()
        } else {
            format!("Create, edit and delete \"{collection}\" records in your public repository")
        };
    }
    if let Some(pattern) = scope.strip_prefix("blob:") {
        return match pattern {
            "*/*" => "Upload files of any type".to_string(),
            p => match p.strip_suffix("/*") {
                Some(kind) => format!("Upload {kind} files"),
                None => format!("Upload \"{p}\" files"),
            },
        };
    }
    if let Some(rest) = scope.strip_prefix("rpc:")
        && let Some((lxm_pat, query)) = rest.split_once('?')
        && let Some(aud_pat) = query.strip_prefix("aud=")
    {
        let service = match percent_decode(aud_pat) {
            a if a == "*" => "other ATProto services".to_string(),
            a if same_did(&a, APPVIEW_SERVICE_DID) => "the Bluesky AppView".to_string(),
            a if same_did(&a, CHAT_SERVICE_DID) => "the Bluesky chat service".to_string(),
            a => a,
        };
        return if lxm_pat == "*" {
            format!("Contact {service} on your behalf")
        } else {
            format!("Call \"{lxm_pat}\" at {service} on your behalf")
        };
    }
    scope
}

/// The app-credential plane: the ecosystem's app-password semantics.
fn authorize_app_credential(input: &AuthzInput, class: LexiconClass, dm: bool) -> AuthzVerdict {
    // Exactly one scope, and it must be one this plane mints. Anything else is
    // an internal inconsistency (a verified token carrying a scope we never
    // issue) — refuse uniformly rather than guess.
    let dm_allowed = match input.scopes.as_slice() {
        [s] if s == SCOPE_APP_PASS => false,
        [s] if s == SCOPE_APP_PASS_PRIVILEGED => true,
        _ => return AuthzVerdict::uniform(),
    };
    implicit_grant(class, dm, dm_allowed)
}

/// The implicit grant an app-credential session carries (`:195`), shared with
/// the OAuth plane's transition scopes because they are *defined* as it.
fn implicit_grant(class: LexiconClass, dm: bool, dm_allowed: bool) -> AuthzVerdict {
    if dm && !dm_allowed {
        return AuthzVerdict::deny(
            ERR_INVALID_TOKEN,
            "this credential is not authorized for direct messages; mint one with \
             direct-message access from your Fauna app",
        );
    }
    match class {
        LexiconClass::Read
        | LexiconClass::Write
        | LexiconClass::Blob
        | LexiconClass::SessionIdentity
        | LexiconClass::SessionLifecycle
        | LexiconClass::Preferences
        | LexiconClass::Dm
        | LexiconClass::AppView
        | LexiconClass::ServiceAuthMint => AuthzVerdict::Allow,
        LexiconClass::AccountMutation => AuthzVerdict::deny(
            ERR_INVALID_TOKEN,
            "account and credential management happens in your Fauna app, not over \
             the ATProto API",
        ),
        LexiconClass::Migration => AuthzVerdict::deny(
            ERR_NOT_IMPLEMENTED,
            "account migration is not yet served by this PDS; repo export via \
             com.atproto.sync.getRepo is already public",
        ),
        LexiconClass::Unknown => AuthzVerdict::deny(
            ERR_INVALID_TOKEN,
            "this method is not covered by the PDS authorization policy",
        ),
    }
}

/// The OAuth plane (F4's caller). Transition scopes map onto the implicit
/// grant; granular scopes are evaluated against the same `lxm`/`aud` inputs.
/// A scope set is **additive**: transitional and granular scopes coexist during
/// the ecosystem's migration, and a grant carrying both must be covered by
/// either. So the transition arm is consulted first and, if it does not cover
/// the call, the granular scopes still get their say — short-circuiting on the
/// transition verdict would falsely deny a grant that holds both.
fn authorize_oauth(input: &AuthzInput, class: LexiconClass, dm: bool) -> AuthzVerdict {
    let has_generic = input.scopes.iter().any(|s| s == "transition:generic");
    let has_chat = input.scopes.iter().any(|s| s == "transition:chat.bsky");
    // `transition:email` deliberately contributes nothing — Fauna has no email
    // account model for it to grant over.
    let transition = (has_generic || has_chat).then(|| implicit_grant(class, dm, has_chat));
    if matches!(transition, Some(AuthzVerdict::Allow)) {
        return AuthzVerdict::Allow;
    }
    if input
        .scopes
        .iter()
        .any(|s| granular_scope_permits(s, input, class))
    {
        return AuthzVerdict::Allow;
    }
    // Prefer the transition arm's refusal when there was one: it names the
    // actual reason (a missing DM class, a refused account op) where the
    // granular fallback can only say "nothing covered it".
    transition.unwrap_or_else(|| {
        AuthzVerdict::deny(
            ERR_INVALID_TOKEN,
            "no granted scope covers this method and audience",
        )
    })
}

/// The classes **no** granular OAuth scope reaches, whatever it names.
///
/// This is the executable form of the ratified sentence in
/// `atproto-pds-full.md` § F4 detail's *Scope model* — "no granular OAuth scope
/// reaches them at all". It sits here, above every arm, rather than inside one,
/// for two reasons: the claim is about the granular *family*, and the next arm
/// this grammar grows (`include:` permission sets, the deferred Lexicon-
/// resolution track) inherits it by construction instead of having to remember.
///
/// **Why it had to become a gate at all** (settled
/// 2026-07-31, with its mechanism corrected). The `rpc:` arm below is
/// **class-blind** — it matches an `lxm` pattern and an `aud` pattern and never
/// asks what kind of operation it just permitted. So `rpc:*?aud=*` returned
/// `true` for an [`LexiconClass::AccountMutation`] or
/// [`LexiconClass::SessionLifecycle`] call the moment anything supplied an
/// `aud`, and the pin asserting the lifecycle boundary
/// (`oauth_metadata::tests::no_granular_oauth_scope_reaches_the_session_lifecycle_verbs`)
/// passed only because its probe left `aud` at `None` — a second, unrelated
/// condition doing the work the pin named (the vacuous-pin class, second
/// sighting). The class split alone could never have delivered the property:
/// splitting a class only helps an arm that *reads* the class.
///
/// **What is deliberately NOT in here, and why the arm stays class-blind for
/// the rest.** An `rpc:` scope authorizes acting *toward a named audience* —
/// the effect lands at `<aud>`, not on this box — so for most classes the
/// question "what would this lexicon do if we served it" is simply the wrong
/// one. [`LexiconClass::Unknown`] in particular **must** stay permitted: a
/// third-party service (a labeler, a custom feed generator) defines its own
/// NSIDs, and `Unknown` is exactly what "this PDS has no local policy for a
/// method it is about to forward" looks like. Denying it would break service
/// proxying, which is the `rpc:` family's whole purpose.
///
/// The three named here are the ones whose subject is **this box's own account
/// and credential plane**, where a remote audience does not make the operation
/// someone else's business:
///
/// * [`LexiconClass::SessionLifecycle`] — the plane boundary, ratified above.
/// * [`LexiconClass::AccountMutation`] — D6: account and credential management
///   lives in the Fauna apps. `implicit_grant` refuses it for the app-credential
///   plane, and a *granular* scope must not end up wider than the app-password
///   grant on the one class that plane exists to withhold.
/// * [`LexiconClass::Migration`] — deferred, never policy-refused. A grammar
///   that refuses it everywhere except through one class-blind arm states the
///   deferral inconsistently.
///
/// The `match` is exhaustive on purpose: a new [`LexiconClass`] variant is a
/// compile error here, so the next session that grows the matrix decides this
/// question instead of inheriting whichever answer the arm order happened to
/// give.
fn no_granular_scope_reaches(class: LexiconClass) -> bool {
    match class {
        LexiconClass::SessionLifecycle
        | LexiconClass::AccountMutation
        | LexiconClass::Migration => true,
        LexiconClass::Read
        | LexiconClass::Write
        | LexiconClass::Blob
        | LexiconClass::SessionIdentity
        | LexiconClass::Preferences
        | LexiconClass::Dm
        | LexiconClass::AppView
        | LexiconClass::ServiceAuthMint
        | LexiconClass::Unknown => false,
    }
}

/// Does one scope from the OAuth grammar cover this call — the base
/// [`SCOPE_ATPROTO_BASE`] or any granular scope?
///
/// The DM class needs no separate gate here: a `chat.bsky.*` call is reached
/// only through an `rpc:` scope whose `aud` clause names the chat service, so
/// the grammar carries its own audience gate.
fn granular_scope_permits(scope: &str, input: &AuthzInput, class: LexiconClass) -> bool {
    // Before any arm gets a say: the classes the granular family never reaches,
    // whatever a scope names. See [`no_granular_scope_reaches`] for why this is
    // here rather than inside the one arm that needed it.
    if no_granular_scope_reaches(class) {
        return false;
    }
    // `atproto` — the spec-mandatory BASE scope, present in every request.
    //
    // What it grants is a **policy decision this PDS owns**, and the answer is
    // the narrowest one that leaves an `atproto`-only grant usable: the
    // `SessionIdentity` class, i.e. basic account identity — who am I
    // (`getSession`) and what is this server (`describeServer`).
    //
    // Narrow deliberately, in both directions:
    //
    // * It must grant **something**. Every request carries it, so a base scope
    //   that covered nothing would make a spec-compliant `atproto`-only grant
    //   deny `getSession` — a client could authenticate and then not be
    //   allowed to ask who it had authenticated as. That was the state this
    //   module shipped in until F4 slice 2.
    // * It must not grant **more**. Reads, writes, blobs, DMs, the AppView and
    //   service-auth minting each have a granular scope whose whole purpose is
    //   to say so explicitly; folding any of them into the base scope would
    //   make the grammar decorative — a client would receive them without
    //   asking, and the consent screen would understate what it granted.
    // * **And it does not carry session lifecycle**. `createSession`/`refreshSession`/`deleteSession`
    //   belong to the app-credential plane; the OAuth plane's lifecycle is
    //   `/oauth/token` and `/oauth/revoke`. That makes it a plane boundary
    //   rather than a scope width — which is why NO granular scope reaches
    //   [`LexiconClass::SessionLifecycle`] either, not just this one.
    //
    // Note this is additive with the transition arm rather than a fallback for
    // it: a modern grant carries `atproto` *plus* granular scopes, and each is
    // asked independently.
    if scope == SCOPE_ATPROTO_BASE {
        return class == LexiconClass::SessionIdentity;
    }
    // rpc:<lxm|*>?aud=<did|*> — service proxying and getServiceAuth minting.
    //
    // ⚠ This arm is **class-blind by design**, and the gate above is what makes
    // that safe rather than accidental. Its two mechanisms both export the
    // effect: `proxyForward` forwards the request to `<aud>` (minting a fresh
    // service-auth token under the caller's own repo key as it goes —
    // `internal/atprotopds/proxy.go`), and `getServiceAuth` hands the caller a
    // token only `<aud>` can spend. In both, what the lexicon would *do locally*
    // is not this box's question — which is precisely why the classes that ARE
    // this box's question had to be lifted out above rather than added here.
    //
    // Note `getServiceAuth` refutes the tempting shorthand "no locally-served
    // route ever supplies `aud`": it is served locally and supplies one
    // deliberately, through `AuthorizeServiceAuth`'s second check (§ F3 detail's
    // two-checks-one-matrix ruling). "Locally served" is not the axis; "whose
    // plane does this operate on" is.
    if let Some(rest) = scope.strip_prefix("rpc:") {
        let Some((lxm_pat, query)) = rest.split_once('?') else {
            return false;
        };
        let Some(aud_pat) = query.strip_prefix("aud=") else {
            return false;
        };
        if lxm_pat != "*" && lxm_pat != input.lxm {
            return false;
        }
        let Some(aud) = input.aud.as_deref() else {
            return false;
        };
        let aud_pat = percent_decode(aud_pat);
        return aud_pat == "*" || same_did(&aud_pat, aud);
    }
    // repo:<collection|*> — only the wildcard is evaluable from the pinned
    // input set: a collection-narrowed scope needs the record's collection,
    // which lives in the request body and the frame does not carry today.
    // Closed world says deny until F4 plumbs it in.
    if let Some(rest) = scope.strip_prefix("repo:") {
        return rest == "*" && matches!(class, LexiconClass::Read | LexiconClass::Write);
    }
    // blob:<mime-pattern> — gates blob upload only. The MIME pattern is
    // checked at the handler, which is where the content type is known.
    if scope.starts_with("blob:") {
        return class == LexiconClass::Blob;
    }
    // `account:` and `identity:` parse but grant nothing on this PDS: account
    // management and handle authority live in Fauna (D6 policy-refused), so
    // there is no surface for them to open. Recognized-and-empty, not unknown.
    false
}

/// May a manifest's `service_auth` entry name `lxm` at all — the deny half
/// of `third-party.md` § The manifest. The bridge-held custodian never mints a
/// service-auth token for account mutation, migration, session lifecycle or
/// the mint itself, so a manifest declaring one refuses at resolution (a
/// publisher learns at publication, never at the first call).
///
/// A method this module has not classified is admitted unless it sits in
/// `com.atproto.*` — the PDS's own namespace, where a new endpoint may be any
/// of the four and so fails closed until a session classifies it. Every other
/// namespace is a service's own lexicon (an AppView's, a feed generator's),
/// which is what service auth exists to reach.
#[must_use]
pub fn service_auth_lxm_admitted(lxm: &str) -> bool {
    match classify(lxm) {
        LexiconClass::AccountMutation
        | LexiconClass::Migration
        | LexiconClass::SessionLifecycle
        | LexiconClass::ServiceAuthMint => false,
        LexiconClass::Unknown => !lxm.starts_with("com.atproto."),
        LexiconClass::Read
        | LexiconClass::Write
        | LexiconClass::Blob
        | LexiconClass::SessionIdentity
        | LexiconClass::Preferences
        | LexiconClass::Dm
        | LexiconClass::AppView => true,
    }
}

/// Classify an `lxm`. The `com.atproto.sync.*` and `app.bsky.*` prefixes are
/// themselves classifications (both are surfaces the ecosystem grows
/// continuously and the grant genuinely covers wholesale) — everything else
/// must be named, so a new `com.atproto.server.*` endpoint lands in `Unknown`
/// and denies until a session classifies it.
fn classify(lxm: &str) -> LexiconClass {
    if lxm.starts_with("chat.bsky.") {
        return LexiconClass::Dm;
    }
    match lxm {
        "com.atproto.server.createSession"
        | "com.atproto.server.refreshSession"
        | "com.atproto.server.deleteSession" => LexiconClass::SessionLifecycle,

        "com.atproto.server.getSession" | "com.atproto.server.describeServer" => {
            LexiconClass::SessionIdentity
        }

        "com.atproto.server.getServiceAuth" => LexiconClass::ServiceAuthMint,

        "com.atproto.repo.getRecord"
        | "com.atproto.repo.listRecords"
        | "com.atproto.repo.describeRepo"
        | "com.atproto.identity.resolveHandle"
        | "com.atproto.identity.resolveDid"
        | "com.atproto.identity.resolveIdentity" => LexiconClass::Read,

        "com.atproto.repo.createRecord"
        | "com.atproto.repo.putRecord"
        | "com.atproto.repo.deleteRecord"
        | "com.atproto.repo.applyWrites" => LexiconClass::Write,

        "com.atproto.repo.uploadBlob" => LexiconClass::Blob,

        // DEFERRED, never policy — see LexiconClass::Migration.
        "com.atproto.server.activateAccount"
        | "com.atproto.server.deactivateAccount"
        | "com.atproto.server.checkAccountStatus"
        | "com.atproto.server.reserveSigningKey"
        | "com.atproto.repo.importRepo" => LexiconClass::Migration,

        "com.atproto.server.createAccount"
        | "com.atproto.server.deleteAccount"
        | "com.atproto.server.requestAccountDelete"
        | "com.atproto.server.createAppPassword"
        | "com.atproto.server.listAppPasswords"
        | "com.atproto.server.revokeAppPassword"
        | "com.atproto.server.updateEmail"
        | "com.atproto.server.requestEmailUpdate"
        | "com.atproto.server.requestEmailConfirmation"
        | "com.atproto.server.confirmEmail"
        | "com.atproto.server.requestPasswordReset"
        | "com.atproto.server.resetPassword"
        | "com.atproto.identity.updateHandle"
        | "com.atproto.identity.requestPlcOperationSignature"
        | "com.atproto.identity.signPlcOperation"
        | "com.atproto.identity.submitPlcOperation" => LexiconClass::AccountMutation,

        "app.bsky.actor.getPreferences" | "app.bsky.actor.putPreferences" => {
            LexiconClass::Preferences
        }

        _ if lxm.starts_with("com.atproto.sync.") => LexiconClass::Read,
        _ if lxm.starts_with("app.bsky.") => LexiconClass::AppView,
        _ => LexiconClass::Unknown,
    }
}

/// Compare two service DIDs ignoring the `#fragment` — callers write the
/// audience both ways and the DID is what identifies the service.
fn same_did(a: &str, b: &str) -> bool {
    fn did_part(s: &str) -> &str {
        s.split('#').next().unwrap_or(s)
    }
    did_part(a) == did_part(b)
}

/// Minimal percent-decoding for scope `aud=` clauses (`%23` → `#` is the one
/// that matters in practice). No `+`-as-space handling — a literal `+` in an
/// `aud=` clause passes through unchanged.
pub(crate) fn percent_decode(s: &str) -> String {
    fauna_core::web::percent_decode(s, false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `third-party.md` § The manifest: the methods a `service_auth` entry may
    /// never name, and the service lexicons it exists to reach.
    #[test]
    fn service_auth_admits_service_lexicons_and_never_the_custodians_own_surface() {
        for denied in [
            "com.atproto.identity.submitPlcOperation",
            "com.atproto.identity.signPlcOperation",
            "com.atproto.server.deleteAccount",
            "com.atproto.server.createSession",
            "com.atproto.server.getServiceAuth",
            "com.atproto.repo.importRepo",
            // Unclassified, in the PDS's own namespace: fail closed.
            "com.atproto.server.someFutureEndpoint",
        ] {
            assert!(!service_auth_lxm_admitted(denied), "{denied}");
        }
        for admitted in [
            "app.bsky.feed.getFeedSkeleton",
            "chat.bsky.convo.getLog",
            "com.atproto.repo.getRecord",
            "com.example.blog.getPost",
        ] {
            assert!(service_auth_lxm_admitted(admitted), "{admitted}");
        }
    }

    /// The app plane's ordinary session against a plain read.
    fn app(lxm: &str) -> AuthzInput {
        AuthzInput {
            plane: PLANE_APP_CREDENTIAL.into(),
            scopes: vec![SCOPE_APP_PASS.into()],
            external_apps_enabled: true,
            lxm: lxm.into(),
            aud: None,
            endpoint_class: ENDPOINT_CLASS_AUTHED.into(),
        }
    }

    fn privileged(lxm: &str) -> AuthzInput {
        AuthzInput {
            scopes: vec![SCOPE_APP_PASS_PRIVILEGED.into()],
            ..app(lxm)
        }
    }

    fn deny_name(v: &AuthzVerdict) -> String {
        match v {
            AuthzVerdict::Allow => panic!("expected a Deny, got Allow"),
            AuthzVerdict::Deny { xrpc_error, .. } => xrpc_error.clone(),
        }
    }

    // ── The app-credential implicit grant (:195) ─────────────────────────────

    #[test]
    fn app_credential_grant_covers_repo_read_write_blob_and_appview() {
        for lxm in [
            "com.atproto.repo.getRecord",
            "com.atproto.repo.listRecords",
            "com.atproto.repo.describeRepo",
            "com.atproto.sync.getRepo",
            "com.atproto.identity.resolveHandle",
            "com.atproto.repo.createRecord",
            "com.atproto.repo.putRecord",
            "com.atproto.repo.deleteRecord",
            "com.atproto.repo.applyWrites",
            "com.atproto.repo.uploadBlob",
            "com.atproto.server.getSession",
            "app.bsky.actor.getPreferences",
            "app.bsky.actor.putPreferences",
            "app.bsky.feed.getTimeline",
            "app.bsky.graph.getFollows",
            "com.atproto.server.getServiceAuth",
        ] {
            assert!(
                authorize(app(lxm)).is_allow(),
                "app credential must reach {lxm}"
            );
        }
    }

    #[test]
    fn app_credential_grant_excludes_the_account_mutation_class() {
        for lxm in [
            "com.atproto.server.createAccount",
            "com.atproto.server.deleteAccount",
            "com.atproto.server.requestAccountDelete",
            "com.atproto.server.createAppPassword",
            "com.atproto.server.listAppPasswords",
            "com.atproto.server.revokeAppPassword",
            "com.atproto.server.updateEmail",
            "com.atproto.server.requestPasswordReset",
            "com.atproto.identity.updateHandle",
            "com.atproto.identity.signPlcOperation",
            "com.atproto.identity.submitPlcOperation",
        ] {
            assert_eq!(
                deny_name(&authorize(app(lxm))),
                ERR_INVALID_TOKEN,
                "{lxm} must be refused to an app credential"
            );
        }
    }

    #[test]
    fn migration_endpoints_are_deferred_never_policy() {
        for lxm in [
            "com.atproto.server.activateAccount",
            "com.atproto.server.deactivateAccount",
            "com.atproto.server.checkAccountStatus",
            "com.atproto.repo.importRepo",
        ] {
            let v = authorize(app(lxm));
            assert_eq!(deny_name(&v), ERR_NOT_IMPLEMENTED, "{lxm} must be deferred");
            let AuthzVerdict::Deny { message, .. } = &v else {
                unreachable!()
            };
            assert!(
                message.contains("not yet"),
                "the deferred message must say 'not yet', got {message:?}"
            );
        }
    }

    // ── The DM class (:195) ──────────────────────────────────────────────────

    #[test]
    fn dm_requires_the_privileged_scope_by_lxm() {
        assert_eq!(
            deny_name(&authorize(app("chat.bsky.convo.listConvos"))),
            ERR_INVALID_TOKEN
        );
        assert!(authorize(privileged("chat.bsky.convo.listConvos")).is_allow());
    }

    #[test]
    fn dm_requires_the_privileged_scope_by_aud_even_for_an_innocuous_lxm() {
        // The audience is what makes it DM traffic — an ordinary-looking lxm
        // aimed at the chat service must not slip past on its name.
        let mut i = app("app.bsky.feed.getTimeline");
        i.aud = Some(CHAT_SERVICE_DID.into());
        assert_eq!(deny_name(&authorize(i)), ERR_INVALID_TOKEN);

        let mut p = privileged("app.bsky.feed.getTimeline");
        p.aud = Some(CHAT_SERVICE_DID.into());
        assert!(authorize(p).is_allow());
    }

    #[test]
    fn chat_aud_matches_with_or_without_the_service_fragment() {
        for aud in ["did:web:api.bsky.chat", CHAT_SERVICE_DID] {
            let mut i = app("app.bsky.feed.getTimeline");
            i.aud = Some(aud.into());
            assert_eq!(deny_name(&authorize(i)), ERR_INVALID_TOKEN, "aud {aud}");
        }
    }

    #[test]
    fn appview_proxying_needs_no_privileged_scope() {
        let mut i = app("app.bsky.feed.getTimeline");
        i.aud = Some(APPVIEW_SERVICE_DID.into());
        assert!(authorize(i).is_allow());
    }

    // ── getServiceAuth: both checks run through the same matrix (:193) ───────

    #[test]
    fn get_service_auth_second_check_defers_migration_lxms() {
        // Check 1: may this credential mint at all?
        assert!(authorize(app("com.atproto.server.getServiceAuth")).is_allow());
        // Check 2: may it mint for *this* method? Migration stays deferred.
        let mut i = app("com.atproto.repo.importRepo");
        i.aud = Some("did:web:pds.example.com".into());
        assert_eq!(deny_name(&authorize(i)), ERR_NOT_IMPLEMENTED);
    }

    #[test]
    fn get_service_auth_second_check_gates_dm_minting_on_the_scope() {
        let mut plain = app("chat.bsky.convo.sendMessage");
        plain.aud = Some(CHAT_SERVICE_DID.into());
        assert_eq!(deny_name(&authorize(plain)), ERR_INVALID_TOKEN);

        let mut priv_ = privileged("chat.bsky.convo.sendMessage");
        priv_.aud = Some(CHAT_SERVICE_DID.into());
        assert!(authorize(priv_).is_allow());
    }

    // ── The kill-switch input, and its uniformity (:192) ─────────────────────

    #[test]
    fn kill_switch_off_denies_everything_uniformly() {
        for lxm in [
            "com.atproto.repo.getRecord",
            "com.atproto.repo.createRecord",
            "app.bsky.feed.getTimeline",
            "com.atproto.server.getSession",
        ] {
            let i = AuthzInput {
                external_apps_enabled: false,
                ..app(lxm)
            };
            let v = authorize(i);
            assert_eq!(deny_name(&v), ERR_AUTH_REQUIRED, "{lxm}");
            // Byte-identical to the unrecognized-plane refusal: a disabled
            // account must not be distinguishable from a bad token.
            let unknown = authorize(AuthzInput {
                plane: "martian".into(),
                ..app(lxm)
            });
            assert_eq!(v, unknown, "kill-switch deny must be the uniform refusal");
        }
    }

    #[test]
    fn kill_switch_outranks_the_privileged_scope() {
        let i = AuthzInput {
            external_apps_enabled: false,
            ..privileged("chat.bsky.convo.listConvos")
        };
        assert_eq!(deny_name(&authorize(i)), ERR_AUTH_REQUIRED);
    }

    // ── Closed world: every unrecognized input denies (:194) ─────────────────

    #[test]
    fn unknown_plane_denies() {
        assert_eq!(
            deny_name(&authorize(AuthzInput {
                plane: "".into(),
                ..app("com.atproto.repo.getRecord")
            })),
            ERR_AUTH_REQUIRED
        );
    }

    #[test]
    fn unparseable_app_scope_denies_uniformly() {
        for scopes in [
            vec![],
            vec!["com.atproto.refresh".to_string()],
            vec!["transition:generic".to_string()],
            vec![SCOPE_APP_PASS.to_string(), SCOPE_APP_PASS.to_string()],
        ] {
            let i = AuthzInput {
                scopes,
                ..app("com.atproto.repo.getRecord")
            };
            assert_eq!(deny_name(&authorize(i)), ERR_AUTH_REQUIRED);
        }
    }

    #[test]
    fn unclassified_lxm_denies() {
        for lxm in [
            "com.example.somethingNew",
            "com.atproto.server.someEndpointAddedIn2027",
            "",
        ] {
            assert_eq!(
                deny_name(&authorize(app(lxm))),
                ERR_INVALID_TOKEN,
                "unclassified {lxm} must deny"
            );
        }
    }

    /// The write class is the frame's `xrpc.ClassWrite`. It must stay in the
    /// accepted set: dropping it would not fail a build on either side — Go
    /// would keep sending `"write"` and every external repo write would deny
    /// with the *uniform* refusal, which reads as a bad token rather than as
    /// the closed world rejecting a class it was never taught.
    #[test]
    fn the_write_endpoint_class_is_accepted() {
        for lxm in [
            "com.atproto.repo.createRecord",
            "com.atproto.repo.putRecord",
            "com.atproto.repo.deleteRecord",
            "com.atproto.repo.applyWrites",
        ] {
            let i = AuthzInput {
                endpoint_class: ENDPOINT_CLASS_WRITE.into(),
                ..app(lxm)
            };
            assert_eq!(authorize(i), AuthzVerdict::Allow, "{lxm} under write class");
        }
    }

    /// The same two-sided-constant contract for `blob` (F2.4 slice 1). A blob
    /// upload is its own class rather than `write`'s because a record write
    /// costs a funnel commit of a few KB while a blob body runs to the ceiling —
    /// and because an image post is `uploadBlob` *then* `createRecord`, so
    /// sharing one bucket would silently halve the post rate for exactly the
    /// callers who upload media. Both planes must reach it.
    #[test]
    fn the_blob_endpoint_class_is_accepted_on_both_planes() {
        let app_side = AuthzInput {
            endpoint_class: ENDPOINT_CLASS_BLOB.into(),
            ..app("com.atproto.repo.uploadBlob")
        };
        assert_eq!(authorize(app_side), AuthzVerdict::Allow, "app plane");

        let oauth_side = AuthzInput {
            endpoint_class: ENDPOINT_CLASS_BLOB.into(),
            ..oauth(&["blob:*/*"], "com.atproto.repo.uploadBlob")
        };
        assert_eq!(authorize(oauth_side), AuthzVerdict::Allow, "oauth plane");
    }

    /// The class is admitted, not blanket-allowing: a credential whose grant
    /// does not cover blobs still denies on it, so the new class cannot become
    /// an accidental bypass of the scope matrix.
    #[test]
    fn the_blob_endpoint_class_does_not_bypass_the_scope_matrix() {
        let i = AuthzInput {
            endpoint_class: ENDPOINT_CLASS_BLOB.into(),
            ..oauth(&["repo:app.bsky.feed.post"], "com.atproto.repo.uploadBlob")
        };
        assert_eq!(deny_name(&authorize(i)), ERR_INVALID_TOKEN);
    }

    #[test]
    fn unknown_endpoint_class_denies() {
        let i = AuthzInput {
            endpoint_class: "a_class_the_frame_grew_later".into(),
            ..app("com.atproto.repo.getRecord")
        };
        assert_eq!(deny_name(&authorize(i)), ERR_AUTH_REQUIRED);
    }

    // ── OAuth arm (:196) — defined now, F4 is the caller ─────────────────────

    fn oauth(scopes: &[&str], lxm: &str) -> AuthzInput {
        AuthzInput {
            plane: PLANE_OAUTH.into(),
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            external_apps_enabled: true,
            lxm: lxm.into(),
            aud: None,
            endpoint_class: ENDPOINT_CLASS_AUTHED.into(),
        }
    }

    #[test]
    fn transition_generic_maps_onto_the_app_plane_matrix_minus_dm() {
        assert!(
            authorize(oauth(
                &["transition:generic"],
                "com.atproto.repo.createRecord"
            ))
            .is_allow()
        );
        assert!(authorize(oauth(&["transition:generic"], "app.bsky.feed.getTimeline")).is_allow());
        assert_eq!(
            deny_name(&authorize(oauth(
                &["transition:generic"],
                "chat.bsky.convo.listConvos"
            ))),
            ERR_INVALID_TOKEN
        );
        assert_eq!(
            deny_name(&authorize(oauth(
                &["transition:generic"],
                "com.atproto.server.createAppPassword"
            ))),
            ERR_INVALID_TOKEN
        );
    }

    #[test]
    fn transition_chat_adds_the_dm_class() {
        assert!(
            authorize(oauth(
                &["transition:generic", "transition:chat.bsky"],
                "chat.bsky.convo.listConvos"
            ))
            .is_allow()
        );
    }

    // A grant may carry both families during the ecosystem's migration; either
    // covering the call is enough. Short-circuiting on the transition verdict
    // would falsely deny here.
    #[test]
    fn a_granular_scope_still_counts_when_a_transition_scope_does_not_cover_the_call() {
        let mut i = oauth(
            &["transition:generic", "rpc:chat.bsky.convo.listConvos?aud=*"],
            "chat.bsky.convo.listConvos",
        );
        i.aud = Some(CHAT_SERVICE_DID.into());
        assert!(
            authorize(i).is_allow(),
            "transition:generic alone refuses DMs, but the granular rpc scope covers it"
        );
    }

    // When neither family covers it, the transition arm's refusal is the one
    // worth reporting — it names the reason.
    #[test]
    fn a_mixed_grant_that_covers_nothing_reports_the_transition_reason() {
        let v = authorize(oauth(
            &["transition:generic", "rpc:app.bsky.feed.getTimeline?aud=*"],
            "chat.bsky.convo.listConvos",
        ));
        let AuthzVerdict::Deny { message, .. } = &v else {
            panic!("expected a Deny, got {v:?}")
        };
        assert!(
            message.contains("direct messages"),
            "want the DM-specific reason, got {message:?}"
        );
    }

    #[test]
    fn transition_email_grants_nothing() {
        // No email account model exists in Fauna — the scope is meaningless here.
        assert_eq!(
            deny_name(&authorize(oauth(
                &["transition:email"],
                "com.atproto.repo.getRecord"
            ))),
            ERR_INVALID_TOKEN
        );
    }

    #[test]
    fn granular_rpc_scope_matches_on_lxm_and_aud() {
        let scope = "rpc:app.bsky.feed.getTimeline?aud=did:web:api.bsky.app%23bsky_appview";
        let mut ok = oauth(&[scope], "app.bsky.feed.getTimeline");
        ok.aud = Some(APPVIEW_SERVICE_DID.into());
        assert!(authorize(ok).is_allow());

        // Right method, wrong audience.
        let mut wrong_aud = oauth(&[scope], "app.bsky.feed.getTimeline");
        wrong_aud.aud = Some(CHAT_SERVICE_DID.into());
        assert_eq!(deny_name(&authorize(wrong_aud)), ERR_INVALID_TOKEN);

        // Right audience, wrong method.
        let mut wrong_lxm = oauth(&[scope], "app.bsky.feed.getAuthorFeed");
        wrong_lxm.aud = Some(APPVIEW_SERVICE_DID.into());
        assert_eq!(deny_name(&authorize(wrong_lxm)), ERR_INVALID_TOKEN);
    }

    #[test]
    fn granular_rpc_wildcards() {
        let mut any_method = oauth(
            &["rpc:*?aud=did:web:api.bsky.app%23bsky_appview"],
            "app.bsky.feed.getTimeline",
        );
        any_method.aud = Some(APPVIEW_SERVICE_DID.into());
        assert!(authorize(any_method).is_allow());

        let mut any_aud = oauth(
            &["rpc:app.bsky.feed.getTimeline?aud=*"],
            "app.bsky.feed.getTimeline",
        );
        any_aud.aud = Some(CHAT_SERVICE_DID.into());
        assert!(authorize(any_aud).is_allow());
    }

    /// **The widest granular scope there is cannot reach this box's own account
    /// and credential plane**.
    ///
    /// `rpc:*?aud=*` is the honest maximum this AS advertises, and
    /// the `rpc:` arm is class-blind, so before
    /// [`super::no_granular_scope_reaches`] existed this scope permitted
    /// *anything* the moment an `aud` was present — including
    /// `com.atproto.server.createAccount`, whose only protection was that the
    /// call would be proxied away.
    ///
    /// **Every input here has an audience.** That is the whole point: an
    /// audience-less probe is answered by the arm's `input.aud` check before
    /// the class ever matters, which is exactly how the lifecycle pin over in
    /// `oauth_metadata` managed to be vacuous. A future reader tempted to drop
    /// `aud` from these inputs would be deleting the test's ability to see its
    /// own subject.
    ///
    /// The two audiences that reach this in production are `ProxyFallback`'s
    /// (any unknown NSID carrying a proxy target — `internal/atprotopds/proxy.go`)
    /// and `AuthorizeServiceAuth`'s (the caller's *requested* lxm and aud, on a
    /// route that is served locally — which is why "locally served" is not the
    /// axis this gate keys on).
    #[test]
    fn the_widest_rpc_scope_cannot_reach_account_credential_or_migration_lexicons() {
        for lxm in [
            // AccountMutation — the trigger case. Not a registered
            // route, so it lands on the Proxyable fallback with an `aud`.
            "com.atproto.server.createAccount",
            "com.atproto.server.createAppPassword",
            "com.atproto.server.deleteAccount",
            // SessionLifecycle — the plane boundary.
            "com.atproto.server.createSession",
            "com.atproto.server.refreshSession",
            "com.atproto.server.deleteSession",
            // Migration — deferred everywhere; one class-blind arm must not be
            // the exception that states the deferral inconsistently.
            "com.atproto.server.activateAccount",
            "com.atproto.repo.importRepo",
        ] {
            let mut i = oauth(&["atproto", "rpc:*?aud=*"], lxm);
            i.aud = Some("did:web:audience.example".into());
            assert!(
                !authorize(i).is_allow(),
                "`rpc:*?aud=*` grants `{lxm}` — account, credential and migration \
                 lexicons operate on THIS box's own plane, so naming a remote \
                 audience does not make them someone else's business"
            );
        }
    }

    /// The other half of the gate: it withholds three classes, not everything.
    ///
    /// Without this, tightening [`super::no_granular_scope_reaches`] into
    /// "deny more" would look like an improvement and silently break service
    /// proxying — `Unknown` is what a third-party labeler or feed generator's
    /// own NSID classifies as, and forwarding those is the `rpc:` family's
    /// entire purpose.
    #[test]
    fn the_gate_leaves_proxyable_traffic_including_unknown_nsids_grantable() {
        for lxm in [
            "app.bsky.feed.getTimeline",      // AppView
            "chat.bsky.convo.listConvos",     // Dm
            "com.example.labeler.queryLabel", // Unknown — a third party's own NSID
        ] {
            let mut i = oauth(&["atproto", "rpc:*?aud=*"], lxm);
            i.aud = Some("did:web:audience.example".into());
            assert!(
                authorize(i).is_allow(),
                "`rpc:*?aud=*` must still cover `{lxm}` — this scope exists to \
                 authorize proxied calls, and an unclassified NSID is exactly \
                 what a request this PDS forwards rather than serves looks like"
            );
        }
    }

    #[test]
    fn granular_repo_wildcard_grants_writes_but_a_narrowed_one_defers_to_f4() {
        assert!(authorize(oauth(&["repo:*"], "com.atproto.repo.createRecord")).is_allow());
        // A collection-narrowed scope is not evaluable from the pinned input
        // set (the collection lives in the request body, which the frame does
        // not carry today) — closed world says deny until F4 plumbs it.
        let v = authorize(oauth(
            &["repo:app.bsky.feed.post"],
            "com.atproto.repo.createRecord",
        ));
        assert_eq!(deny_name(&v), ERR_INVALID_TOKEN);
    }

    #[test]
    fn granular_blob_scope_gates_upload_blob() {
        assert!(authorize(oauth(&["blob:*/*"], "com.atproto.repo.uploadBlob")).is_allow());
        assert_eq!(
            deny_name(&authorize(oauth(
                &["repo:*"],
                "com.atproto.repo.uploadBlob"
            ))),
            ERR_INVALID_TOKEN
        );
    }

    #[test]
    fn oauth_plane_still_obeys_the_kill_switch_uniformly() {
        let i = AuthzInput {
            external_apps_enabled: false,
            ..oauth(&["transition:generic"], "com.atproto.repo.getRecord")
        };
        assert_eq!(deny_name(&authorize(i)), ERR_AUTH_REQUIRED);
    }

    #[test]
    fn unparseable_oauth_scopes_deny() {
        for scopes in [
            vec![],
            vec!["nonsense".to_string()],
            vec!["rpc:no-aud-clause".to_string()],
            vec![SCOPE_APP_PASS.to_string()], // an app-plane scope on the OAuth plane
        ] {
            let i = AuthzInput {
                scopes,
                ..oauth(&[], "com.atproto.repo.getRecord")
            };
            assert!(!authorize(i).is_allow(), "must deny");
        }
    }

    /// Every scope this AS advertises must render as something friendlier than
    /// itself on a consent surface — a person cannot audit `rpc:*?aud=*`. The
    /// walk mirrors `every_advertised_scope_grants_something`: it asserts a
    /// description EXISTS, never what it says, so rewording is one edit.
    #[test]
    fn every_advertised_scope_has_a_human_description() {
        for scope in GRANTABLE_SCOPES.iter().chain(OIDC_SCOPES) {
            let description = describe_scope(scope.to_string());
            assert_ne!(
                description, *scope,
                "advertised scope {scope:?} fell through to the verbatim fallback"
            );
        }
    }

    /// A parameterized narrowing must carry its parameter into the rendering —
    /// a card saying only "create records" for `repo:app.bsky.feed.post` would
    /// have the user approve a wider-sounding grant than the one recorded.
    #[test]
    fn narrowed_scopes_render_their_parameter() {
        assert!(describe_scope("repo:app.bsky.feed.post".into()).contains("app.bsky.feed.post"));
        assert!(describe_scope("blob:image/*".into()).contains("image"));
        let narrowed_rpc =
            describe_scope("rpc:com.example.calendar.sync?aud=did%3Aweb%3Acal.example.com".into());
        assert!(
            narrowed_rpc.contains("com.example.calendar.sync"),
            "{narrowed_rpc}"
        );
        assert!(
            narrowed_rpc.contains("did:web:cal.example.com"),
            "{narrowed_rpc}"
        );
    }

    /// The two hard-coded ecosystem services render by name, not by DID — the
    /// one place a raw DID is *less* honest, because these two are the defaults
    /// a headerless client reaches without ever choosing them.
    #[test]
    fn known_service_dids_render_by_name() {
        let appview = describe_scope(format!("rpc:*?aud={APPVIEW_SERVICE_DID}"));
        assert!(appview.contains("AppView"), "{appview}");
        let chat = describe_scope(format!("rpc:*?aud={CHAT_SERVICE_DID}"));
        assert!(chat.contains("chat"), "{chat}");
    }

    /// The OIDC family is one predicate's business like the other two
    /// families (`authorization-server.md` § Scope grammar): every member is
    /// grantable, so PAR accepts it and the discovery document may advertise
    /// it — and nothing merely NAMED like it is.
    #[test]
    fn the_oidc_family_is_grantable_and_exactly_three_strings() {
        for scope in OIDC_SCOPES {
            assert!(scope_grants_something(scope), "{scope} must be grantable");
        }
        for lookalike in [
            "openid:profile",
            "OpenID",
            "profile:*",
            "email:read",
            "phone",
        ] {
            assert!(
                !scope_grants_something(lookalike),
                "{lookalike} is not a member of the fixed OIDC family"
            );
        }
    }

    /// The OIDC scopes grant nothing at the PDS — their grant is the issuer's
    /// own surfaces — so an OIDC-only token presented to the resource server
    /// reaches no method, not even the base scope's identity read.
    #[test]
    fn an_oidc_only_grant_reaches_nothing_at_the_pds() {
        let i = AuthzInput {
            scopes: OIDC_SCOPES.iter().map(|s| s.to_string()).collect(),
            ..oauth(&[], "com.atproto.server.getSession")
        };
        assert!(!authorize(i).is_allow());
    }

    /// `profile` and `email` render as separate rows that say different
    /// things — approving one must not read as approving the other.
    #[test]
    fn the_three_oidc_rows_are_distinct() {
        let rows: std::collections::HashSet<String> = OIDC_SCOPES
            .iter()
            .map(|s| describe_scope(s.to_string()))
            .collect();
        assert_eq!(rows.len(), OIDC_SCOPES.len(), "{rows:?}");
    }

    /// The reader-set table (`authorization-server.md` § The issuer → *The
    /// audience is the set of readers*): the PDS for an ATProto-family scope,
    /// the issuer for an OIDC or Fauna-family one, both when the grant spans
    /// them — and never a reader the grant cannot be exercised at.
    #[test]
    fn the_audience_is_the_set_of_readers_the_grants_families_name() {
        const PDS: &str = "did:web:pds.nest.example";
        const ISS: &str = "https://nest.example";
        let readers = |scopes: &[&str]| {
            let scopes: Vec<String> = scopes.iter().map(|s| s.to_string()).collect();
            access_token_audience(&scopes, PDS, ISS)
        };
        assert_eq!(readers(&["atproto", "transition:generic"]), vec![PDS]);
        assert_eq!(readers(&["openid", "profile", "email"]), vec![ISS]);
        assert_eq!(
            readers(&["fauna:files.read"]),
            vec![ISS],
            "a Fauna-family scope is the nest's, so it must not name the PDS"
        );
        assert_eq!(readers(&["fauna:files.read", "openid"]), vec![ISS]);
        assert_eq!(readers(&["atproto", "openid"]), vec![PDS, ISS]);
        assert_eq!(readers(&["atproto", "fauna:files.read"]), vec![PDS, ISS]);
        // The order is the families', never the scope string's: one grant must
        // not mint two spellings of the same set.
        assert_eq!(readers(&["openid", "atproto"]), vec![PDS, ISS]);
        assert!(readers(&[]).is_empty(), "no scope, no reader");
    }

    /// The three families partition the scope space: every scope belongs to
    /// exactly one, and the ATProto family is what the other two are not.
    #[test]
    fn every_scope_belongs_to_exactly_one_family() {
        for scope in ["atproto", "transition:generic", "rpc:*?aud=*", "openid"]
            .into_iter()
            .chain(["email", "fauna:files.read", "fauna:", "faunax", "openidx"])
        {
            let memberships = [
                is_oidc_scope(scope),
                is_fauna_scope(scope),
                is_atproto_family_scope(scope),
            ];
            assert_eq!(
                memberships.iter().filter(|m| **m).count(),
                1,
                "{scope}: {memberships:?}"
            );
        }
        assert!(is_fauna_scope("fauna:files.read"));
        assert!(!is_fauna_scope("faunax"), "the prefix includes the colon");
    }

    /// The Fauna family answers by arm (`authorization-server.md` § Scope
    /// grammar → *The Fauna family, exactly*): a built arm grants something
    /// and renders its own card row; a well-formed string matching no arm
    /// grants nothing and never reaches the PDS probes.
    #[test]
    fn the_fauna_family_grants_exactly_its_built_arms() {
        for arm in crate::fauna_scope::FaunaScopeArm::ALL {
            for s in arm.examples() {
                assert!(scope_grants_something(s));
                assert_eq!(
                    Some(describe_scope(s.to_string()).as_str()),
                    crate::fauna_scope::card_row(s)
                );
            }
        }
        for unbuilt in [
            "fauna:post:write",
            "fauna:feed:read:home",
            "fauna:files.read",
            "fauna:identity:op",
            "fauna:identity:op:atproto.plc_rotate",
            "fauna:identity:op:identity.succession",
        ] {
            assert!(!scope_grants_something(unbuilt), "{unbuilt}");
        }
        let row = describe_scope(crate::fauna_scope::SCOPE_FEED_READ.to_string());
        assert!(row.contains("no keys are shared"), "{row}");
        let row =
            describe_scope(crate::fauna_scope::SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT.to_string());
        assert!(row.contains("never shared"), "{row}");
    }

    /// The `atproto` base is the ATProto family's entry rule only: a request
    /// naming just the OIDC and Fauna families needs none, and adding any
    /// ATProto-family scope brings the rule back.
    #[test]
    fn only_an_atproto_family_scope_needs_the_base() {
        let set = |s: &[&str]| s.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert!(!lacks_atproto_base(&set(&["fauna:feed:read"])));
        assert!(!lacks_atproto_base(&set(&["fauna:feed:read", "openid"])));
        assert!(lacks_atproto_base(&set(&[
            "fauna:feed:read",
            "transition:generic"
        ])));
        assert!(!lacks_atproto_base(&set(&[
            "fauna:feed:read",
            "atproto",
            "repo:*"
        ])));
    }

    /// Grammar this build does not know falls back to the scope verbatim — an
    /// honest "no friendlier name", never a guess.
    #[test]
    fn unknown_scope_shapes_describe_as_themselves() {
        for scope in [
            "account:email",
            "identity:*",
            "include:com.example.set?aud=*",
        ] {
            assert_eq!(describe_scope(scope.to_string()), scope);
        }
    }
}
