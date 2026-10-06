//! F4 — **pushed authorization requests** (RFC 9126), the pre-consent half of
//! the OAuth flow (`atproto-pds-full.md` § F4 detail, *Endpoints*; § Ecosystem
//! reality item 3, where PAR is a spec hard requirement).
//!
//! PAR inverts the classic authorization request: instead of the client
//! packing its parameters into the browser's redirect, it POSTs them to this
//! server first and receives an opaque `request_uri` to redirect with. The
//! parameters therefore never traverse the user agent, which is what lets the
//! consent screen trust that what it renders is what the client actually
//! asked for.
//!
//! This module is the **decision**: given the posted parameters and the
//! [`ResolvedClient`] they name, may this request be stored at all? Go owns
//! the storage (bridge memory with a TTL, C6) and the `request_uri` minting,
//! for the same reason it owns signing — a random token is an encoding, not a
//! policy.
//!
//! # The ordering that matters
//!
//! Every refusal here happens **before** anything is stored. That is not
//! tidiness: a PAR store is an unauthenticated write surface, so a request
//! that will be rejected must never occupy a slot in it. `crate::oauth_par`
//! being a pure function is what makes the ordering structural rather than a
//! rule Go has to remember — there is no store to write to from in here.
//!
//! # What this module deliberately does not check
//!
//! * **DPoP** — checked, but not here. The proof is a header, not a form
//!   parameter, and it is validated by [`crate::dpop`] *before* the caller
//!   reaches this function at all (before client resolution, in fact, so an
//!   anonymous caller cannot trigger an outbound fetch). What arrives here has
//!   already proved possession of a key.
//! * **Confidential-client authentication** (`private_key_jwt`) — checked, but
//!   not here, for exactly the reason DPoP is not: it is a *form* concern
//!   ([`crate::client_assertion`]) evaluated by the caller **before** this
//!   function, so what arrives here has already proved it is the client it
//!   names. Authenticate, then authorize — a confidential client that has not
//!   shown it is itself must never reach the scope and redirect refusals,
//!   which are diagnostics about *that client's* configuration.
//!
//!   ⚠ **What bounds this endpoint, stated correctly.** *Not* the fact that it
//!   is undiscoverable: `/oauth/par` is a spec-conventional path, so an
//!   attacker finds it without any discovery document, and recording
//!   undiscoverability as the protection would invite a later session to
//!   delete the real bounds on the strength of it. What bounds this endpoint
//!   is the machinery: the per-IP `ClassAuth` limit before the body is read,
//!   the shared fetch guard on its outbound calls, the size and TTL ceilings
//!   on both stores, a DPoP proof with a live server-issued nonce required
//!   *before* client resolution runs, and — since slice 5 — a signed assertion
//!   from any client whose own document says it authenticates.
//! * **Permission sets** (`include:<NSID>`) — the *fetch* is not here, and
//!   cannot be: expanding one resolves a Lexicon document over DNS and HTTPS,
//!   which is Go's half of the split (`atproto-pds-full.md:333`). The decision
//!   is still entirely this module's, in two pieces: [`plan_par_request`]
//!   validates everything a pure function can and hands back the sets to
//!   resolve, and [`finish_par_request`] takes the expansions and decides. The
//!   ordering invariant survives the split — nothing is stored between them,
//!   because Go still has no verdict to store.

use serde::{Deserialize, Serialize};

use crate::authz::{SCOPE_OPENID, is_oidc_scope, lacks_atproto_base, scope_grants_something};
use crate::oauth_client::{
    OAUTH_ERR_INVALID_CLIENT, OAUTH_ERR_INVALID_REQUEST, OAUTH_ERR_INVALID_SCOPE,
    OAUTH_ERR_SERVER_ERROR, OAUTH_ERR_UNSUPPORTED_RESPONSE_TYPE, ResolvedClient,
    redirect_uri_matches, split_scope,
};
use crate::permission_set::{
    ExpandedSet, Expansion, GrantExpansion, IgnoreReason, IgnoredMember, IncludeScope,
    ParsedInclude, check_grant_expansion, check_sets_payload, expand_permission_set,
    include_count_refusal, parse_include_scope,
};

/// The base scope every ATProto authorization request must carry.
use crate::authz::SCOPE_ATPROTO_BASE;

/// The only PKCE challenge method this server accepts.
///
/// `plain` is a downgrade a client would take if offered — and PKCE S256 is a
/// spec hard requirement (§ Ecosystem reality item 3), which is why the AS
/// document advertises `code_challenge_methods_supported: ["S256"]`. Accepting
/// `plain` here would make that advertisement false in the one direction that
/// matters.
pub const CODE_CHALLENGE_METHOD_S256: &str = "S256";

/// A base64url-encoded SHA-256 digest, unpadded: 43 characters. RFC 7636 sets
/// the general `code_verifier`-derived bound at 43–128; an S256 challenge is
/// always exactly 43, and holding that exactly costs nothing and refuses a
/// malformed challenge here rather than at the token endpoint, after the user
/// has already consented.
const S256_CHALLENGE_LEN: usize = 43;

/// The pushed authorization request, as the form body presented it.
///
/// Every field is a `String` rather than a parsed type for the same reason
/// [`crate::authz::AuthzInput`]'s are: an absent or nonsense value must be
/// *representable* so this closed-world check can refuse it, and a Go-side
/// parse would be Go holding policy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ParRequest {
    /// The `client_id` the request names. Must equal the one the resolved
    /// client was resolved from.
    pub client_id: String,
    /// Must be `code`.
    pub response_type: String,
    /// Where the authorization response goes. Must match the client's
    /// declared set (`crate::oauth_client::redirect_uri_matches`).
    pub redirect_uri: String,
    /// Space-delimited, as received.
    pub scope: String,
    /// The client's CSRF token, echoed back on the redirect. Required: without
    /// it the client cannot tie the response to its own request.
    pub state: String,
    /// PKCE challenge.
    pub code_challenge: String,
    /// Must be `S256`.
    pub code_challenge_method: String,
    /// Which account the client believes it is authorizing. Carried, never
    /// trusted — the consent ceremony resolves the actual account from the
    /// authenticated Fauna app that approves, and this only decides which
    /// user's app gets the push.
    pub login_hint: Option<String>,
    /// OIDC's `nonce` (TP6): an opaque value the client binds its session to,
    /// carried from here into the ID token verbatim so the client can tell the
    /// token was minted for THIS sign-in (`authorization-server.md` § OIDC
    /// (TP6)). Optional, as OIDC Core makes it for the code flow; bounded by
    /// [`OIDC_NONCE_MAX_LEN`] because it is stored and echoed.
    pub nonce: Option<String>,
}

/// The longest OIDC `nonce` this server stores and echoes.
///
/// OIDC Core sets no bound, and a nonce is a client's random value — a few
/// dozen characters in every real client. The bound exists because the value
/// is an anonymous caller's, held in the PAR store and copied into a signed
/// token, so it must not be the caller's choice of size.
pub const OIDC_NONCE_MAX_LEN: usize = 256;

/// A request this server is willing to store.
///
/// Deliberately **not** a copy of [`ParRequest`]: the parameters that were
/// only ever gates (`response_type`, `code_challenge_method`) are gone,
/// because carrying a value whose only legal setting was checked here invites
/// a later reader to check it again — differently. What survives is what the
/// consent and token slices need.
///
/// The client identity is *not* duplicated in here either. Go stores the
/// [`ResolvedClient`] beside this, so there is one owner of what the consent
/// screen renders; copying the display members into the request would give a
/// second, staler answer to the same question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct AcceptedParRequest {
    pub client_id: String,
    pub redirect_uri: String,
    /// The grant's **effective** scopes: every entry is one
    /// [`scope_grants_something`] answered for, and the base `atproto` scope is
    /// present whenever any ATProto-family scope is (an OIDC-only sign-in
    /// carries none — see [`plan_par_request`]).
    ///
    /// A permission set contributes its expanded members here and the
    /// `include:` scope itself does **not** appear — the token carries what D8
    /// can act on (`atproto-pds-full.md:333`), and a bare `include:` is not
    /// that. The set's identity survives in [`Self::sets`], which is what the
    /// consent card renders.
    pub scopes: Vec<String>,
    /// Every permission set this request named, expanded — the frozen
    /// expansion (`atproto-pds-full.md:329`).
    ///
    /// Empty for the overwhelmingly common request that names no sets, which is
    /// why it is a list rather than an option: "named none" and "named some
    /// that expanded to nothing" are not the same request, and the second one
    /// never gets this far (it refuses at PAR).
    pub sets: Vec<ExpandedSet>,
    pub state: String,
    /// The S256 challenge, carried verbatim for the token endpoint to verify
    /// the eventual `code_verifier` against.
    pub code_challenge: String,
    pub login_hint: Option<String>,
    /// The OIDC `nonce` the request carried, for the ID token to echo.
    pub nonce: Option<String>,
}

/// A request whose pure half is settled and whose permission sets still have to
/// be fetched.
///
/// It is deliberately not "a partly-built acceptance": nothing in here may be
/// stored, rendered or acted on. It is the argument [`finish_par_request`]
/// needs, and the only thing that turns it into a verdict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct PendingPar {
    /// Everything decided so far. `scopes` holds the **directly requested**
    /// granular scopes only; `sets` is empty until the expansions arrive.
    pub request: AcceptedParRequest,
    /// The sets to resolve, in the order the scope string named them — the
    /// order [`finish_par_request`] expects the expansions back in.
    pub includes: Vec<ParsedInclude>,
}

/// What the pure half decided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ParPlan {
    /// No permission sets: this request is fully decided, store it.
    Accept {
        request: AcceptedParRequest,
    },
    /// Resolve every `pending.includes` entry, expand each, and call
    /// [`finish_par_request`]. **Nothing may be stored before that returns.**
    Resolve {
        pending: PendingPar,
    },
    Deny {
        error: String,
        description: String,
    },
}

impl ParPlan {
    fn deny(error: &str, description: impl Into<String>) -> Self {
        ParPlan::Deny {
            error: error.to_string(),
            description: description.into(),
        }
    }
}

/// The decision. `Deny` carries an RFC 6749 §5.2 error code and a description;
/// Go maps the code to an HTTP status through a mechanical table and emits the
/// pair as the endpoint's JSON error body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ParVerdict {
    Accept { request: AcceptedParRequest },
    Deny { error: String, description: String },
}

impl ParVerdict {
    fn deny(error: &str, description: impl Into<String>) -> Self {
        ParVerdict::Deny {
            error: error.to_string(),
            description: description.into(),
        }
    }
}

/// Validate a pushed authorization request against the client it names, up to
/// the point where a permission set would have to be fetched.
///
/// `client` must be the [`ResolvedClient`] produced from *this request's*
/// `client_id` — the caller resolves first, then validates, and this function
/// re-asserts the pairing rather than trusting it.
///
/// # Why this returns a plan rather than a verdict
///
/// Every refusal a pure function can reach happens **here**, before the caller
/// is told to fetch anything: a request with a bad PKCE challenge, an undeclared
/// scope, a malformed `include:`, or nine permission sets costs zero DNS
/// queries and zero HTTPS requests (`atproto-pds-full.md:331`, the fan-out cap;
/// `:332`, NSID syntax validated before any I/O). Only a request that is
/// otherwise acceptable is worth resolving a third party's document for.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn plan_par_request(request: ParRequest, client: ResolvedClient) -> ParPlan {
    // The pairing. A mismatch means the caller resolved one client and is
    // validating another's request — an internal inconsistency, so it refuses
    // rather than picking a side.
    if request.client_id != client.client_id {
        return ParPlan::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "the request's client_id is not the client that was resolved",
        );
    }

    if request.response_type != "code" {
        return ParPlan::deny(
            OAUTH_ERR_UNSUPPORTED_RESPONSE_TYPE,
            "the only supported response_type is `code`",
        );
    }

    // PKCE, both halves. The method first, so a `plain` challenge is refused
    // as a downgrade by name rather than as a length problem.
    if request.code_challenge_method != CODE_CHALLENGE_METHOD_S256 {
        return ParPlan::deny(
            OAUTH_ERR_INVALID_REQUEST,
            format!("code_challenge_method must be `{CODE_CHALLENGE_METHOD_S256}`"),
        );
    }
    if request.code_challenge.len() != S256_CHALLENGE_LEN
        || !request
            .code_challenge
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return ParPlan::deny(
            OAUTH_ERR_INVALID_REQUEST,
            "code_challenge must be an unpadded base64url SHA-256 digest",
        );
    }

    if request.state.is_empty() {
        return ParPlan::deny(
            OAUTH_ERR_INVALID_REQUEST,
            "state is required — without it the client cannot bind the \
             authorization response to its own request",
        );
    }

    // The redirect URI, against the client's own declared set. RFC 9126 is
    // explicit that a redirect-URI failure must NOT be reported by redirecting
    // (that would be the open redirect itself), so this is a direct error like
    // every other refusal here.
    if !redirect_uri_matches(&client, &request.redirect_uri) {
        return ParPlan::deny(
            OAUTH_ERR_INVALID_REQUEST,
            "redirect_uri is not one this client declared in its metadata",
        );
    }

    let (direct, includes) = match plan_scopes(&client, &request.scope) {
        Ok(planned) => planned,
        Err(deny) => return *deny,
    };
    if let Some(nonce) = &request.nonce
        && (nonce.is_empty() || nonce.len() > OIDC_NONCE_MAX_LEN)
    {
        return ParPlan::deny(
            OAUTH_ERR_INVALID_REQUEST,
            format!("nonce must be between 1 and {OIDC_NONCE_MAX_LEN} characters"),
        );
    }

    let request = AcceptedParRequest {
        client_id: request.client_id,
        redirect_uri: request.redirect_uri,
        scopes: direct,
        sets: Vec::new(),
        state: request.state,
        code_challenge: request.code_challenge,
        login_hint: request.login_hint,
        nonce: request.nonce,
    };
    if includes.is_empty() {
        return ParPlan::Accept { request };
    }
    ParPlan::Resolve {
        pending: PendingPar { request, includes },
    }
}

/// A consent start that has no redirect — the typed code (RFC 8628) and the
/// quiet push (CIBA) — as its form presented it (`authorization-server.md`
/// § Consent).
///
/// Only the parameters a start with no browser leg carries. There is no
/// `redirect_uri`, `state` or PKCE challenge to validate, because nothing
/// travels through a user agent: the handle the client later polls with never
/// leaves the client, and the token exchange is bound to the DPoP key the start
/// was proved under — the property PKCE buys a code that does transit a
/// redirect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StartRequest {
    pub client_id: String,
    /// Space-delimited, as received.
    pub scope: String,
    pub login_hint: Option<String>,
}

/// Validate a redirect-less consent start against the client it names — the
/// **same scope decision** [`plan_par_request`] makes, so a device app and a
/// website cannot be held to two different readings of one client document.
///
/// The accepted request is an [`AcceptedParRequest`] with `redirect_uri`,
/// `state` and `code_challenge` **empty** and no `nonce`, which is the honest
/// value for a start that has none of them, and it lets a `Resolve` plan go through
/// [`finish_par_request`] exactly as PAR's does: permission sets expand one
/// way, whichever door the request came in by.
pub fn plan_start_request(request: StartRequest, client: ResolvedClient) -> ParPlan {
    if request.client_id != client.client_id {
        return ParPlan::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "the request's client_id is not the client that was resolved",
        );
    }
    let (direct, includes) = match plan_scopes(&client, &request.scope) {
        Ok(planned) => planned,
        Err(deny) => return *deny,
    };
    let request = AcceptedParRequest {
        client_id: request.client_id,
        redirect_uri: String::new(),
        scopes: direct,
        sets: Vec::new(),
        state: String::new(),
        code_challenge: String::new(),
        login_hint: request.login_hint,
        // Neither RFC 8628 nor CIBA carries an OIDC `nonce`: an ID token these
        // starts mint names none, which OIDC Core §2 permits for a request
        // that sent none.
        nonce: None,
    };
    if includes.is_empty() {
        return ParPlan::Accept { request };
    }
    ParPlan::Resolve {
        pending: PendingPar { request, includes },
    }
}

/// A refusal shaped as `plan_scopes`'s `Err`. Boxed because [`ParPlan`]'s
/// `Resolve` arm carries a whole [`PendingPar`], which puts the plan well past
/// clippy's `result_large_err` threshold — and a refusal is the rare path.
fn refuse(error: &str, description: impl Into<String>) -> Box<ParPlan> {
    Box::new(ParPlan::deny(error, description))
}

/// The scope half of every consent start: the requested scopes held to the
/// client's own declared set, each one grantable, the base scope present, and
/// the `include:`s split out for resolution under the fan-out cap. `Ok` carries
/// the directly granted scopes and the sets still to resolve; `Err` is the
/// refusal, already shaped as a (boxed) `Deny` plan.
fn plan_scopes(
    client: &ResolvedClient,
    scope: &str,
) -> Result<(Vec<String>, Vec<ParsedInclude>), Box<ParPlan>> {
    let scopes = split_scope(scope);
    if scopes.is_empty() {
        return Err(refuse(OAUTH_ERR_INVALID_SCOPE, "scope is required"));
    }
    // The base `atproto` scope is the ATProto FAMILY's entry requirement, not
    // the server's: a "Sign in with Fauna" request names only the OIDC family
    // (`authorization-server.md` § Scope grammar; `third-party.md`'s first
    // flow asks for `openid profile`), and demanding an ATProto scope of it
    // would put a PDS permission on a card that is about signing in. The
    // moment any other family's scope appears, the base scope is required
    // exactly as before.
    if lacks_atproto_base(&scopes) {
        return Err(refuse(
            OAUTH_ERR_INVALID_SCOPE,
            format!("scope must include the base `{SCOPE_ATPROTO_BASE}` scope"),
        ));
    }
    // `profile` and `email` are claims OF a sign-in: every surface that
    // releases them (the ID token, `/oauth/userinfo`) exists only under
    // `openid`, so without it they would be scopes the user approves and
    // nothing ever honours — the lie `scope_grants_something` exists to keep
    // off the consent card, one family over.
    if !scopes.iter().any(|s| s == SCOPE_OPENID)
        && let Some(orphan) = scopes.iter().find(|s| is_oidc_scope(s))
    {
        return Err(refuse(
            OAUTH_ERR_INVALID_SCOPE,
            format!("`{orphan}` is only granted together with `{SCOPE_OPENID}`"),
        ));
    }

    let mut direct: Vec<String> = Vec::with_capacity(scopes.len());
    let mut includes: Vec<ParsedInclude> = Vec::new();
    for scope in &scopes {
        // The client's own document is its public commitment about what it
        // may ask for. A grant wider than it is one the client's users could
        // not have audited by reading the document — so the subset check is a
        // property of the *client's* accountability, not of ours.
        //
        // Membership is exact-string, deliberately: the ATProto grammar
        // defines no subsumption relation between a declared `rpc:*?aud=*` and
        // a requested `rpc:x?aud=y`, and inventing one here would be this
        // module holding a second opinion about scope meaning — the exact
        // fork D8 exists to prevent. A client that narrows at request time
        // declares what it narrows to.
        //
        // The ONE keyed exception (`authorization-server.md` § Scope grammar →
        // *The folder plane's qualifier is the user's*): an arm whose qualifier
        // is the user's is held by its BARE form, because no static document
        // can declare a row of the user's. The bare declaration admits the
        // bare request (the card chooses the folder) and a qualified one (a
        // re-consent) — each narrows what the document committed to, never
        // widens it.
        let declared = |s: &str| client.declared_scopes.iter().any(|d| d == s);
        if !declared(scope) && !crate::fauna_scope::bare_form(scope).is_some_and(declared) {
            return Err(refuse(
                OAUTH_ERR_INVALID_SCOPE,
                format!("`{scope}` is not a scope this client declared in its metadata"),
            ));
        }
        // An `include:` is held to the declared set above like any other scope
        // — it is the string the client committed to in its own document — but
        // it is not a scope the matrix can answer for, so the grantability
        // check below belongs to its *members*, after expansion.
        match parse_include_scope(scope.clone()) {
            IncludeScope::Parsed { include } => {
                includes.push(include);
                continue;
            }
            IncludeScope::Refused { reason } => {
                // A malformed include refuses the whole request rather than
                // being skipped: the client asked for something this server
                // will not act on, and silently proceeding would mint a grant
                // narrower than the one it believes it received.
                return Err(refuse(OAUTH_ERR_INVALID_SCOPE, reason));
            }
            IncludeScope::NotAnInclude => {}
        }
        // A form-gated arm is refused to a remote-form (confidential) client
        // (`authorization-server.md` § Scope grammar → *The read arm*): TP4
        // grants a remote principal a folder read only over a folder it
        // created, and no door lets one create a folder. Before the
        // grantability check, so the bare plane and verb — a card-qualified
        // request — is refused by this rule, by name, as the qualified
        // string is.
        if client.is_remote_form() && crate::fauna_scope::refused_to_remote_form(scope) {
            return Err(refuse(
                OAUTH_ERR_INVALID_SCOPE,
                format!(
                    "`{scope}` is granted only to an app on the user's own device — a client \
                     that authenticates with its own key cannot read the user's folders"
                ),
            ));
        }
        // …and, independently, one this server can actually honour. Same
        // predicate the discovery documents are pinned by: accepting a scope
        // the matrix denies everywhere would put the lie in the grant and the
        // consent screen instead of in the document. A bare user-qualified
        // string grants nothing as written and is admitted anyway, as a
        // card-qualified request: the user's choice at the ceremony qualifies
        // it before anything is recorded, so no grant ever carries it bare.
        if !scope_grants_something(scope)
            && crate::fauna_scope::user_qualified_bare_arm(scope).is_none()
        {
            return Err(refuse(
                OAUTH_ERR_INVALID_SCOPE,
                format!("`{scope}` is not a scope this authorization server can grant"),
            ));
        }
        // A `records` wildcard covers one publisher's kinds, and only the
        // requesting document's own (`authorization-server.md` § Scope grammar
        // → *Wildcards*): its publisher must be the `client_id`'s host. A
        // single kind of another publisher stays legal here — the louder card,
        // admitted or refused at consent.
        if let Some(qualifier) = crate::fauna_scope::records_qualifier(scope)
            && qualifier.is_wildcard()
            && client_id_host(&client.client_id) != Some(qualifier.publisher())
        {
            return Err(refuse(
                OAUTH_ERR_INVALID_SCOPE,
                format!(
                    "`{scope}` names another publisher's kinds — a wildcard covers only the \
                     kinds of this client's own host"
                ),
            ));
        }
        direct.push(scope.clone());
    }

    // The fan-out cap, before the caller is told to fetch anything.
    if let Some(reason) = include_count_refusal(includes.len() as u32) {
        return Err(refuse(
            OAUTH_ERR_INVALID_SCOPE,
            format!("this authorization request {reason}"),
        ));
    }
    Ok((direct, includes))
}

/// The host a `client_id` URL names, as the `ext.*` grammar spells a
/// publisher (lowercase; no port, no path). `client_id` has already passed
/// `plan_client_id`, so it is an `https` URL or the loopback identity.
fn client_id_host(client_id: &str) -> Option<&str> {
    let rest = client_id
        .strip_prefix("https://")
        .or_else(|| client_id.strip_prefix("http://"))?;
    let end = rest.find(['/', ':', '?', '#']).unwrap_or(rest.len());
    Some(&rest[..end])
}

/// Decide a request whose permission sets have been fetched.
///
/// `records[i]` must be the **verified** `com.atproto.lexicon.schema` record for
/// `pending.includes[i]`, dag-cbor, verbatim as the proof established it.
///
/// # Why this takes bytes rather than expansions
///
/// Expansion is this module's (`atproto-pds-full.md:333`), so the caller has no
/// business performing it and then handing back the result: that would make Go
/// the owner of an intermediate representation and give the ignore rules a
/// second, silent implementation at the seam. Handing back exactly the bytes
/// that were fetched keeps "what was verified" and "what was expanded" the same
/// object across the language boundary, and it means a misaligned answer is
/// caught by [`expand_permission_set`]'s own `id` check — a record served under
/// one NSID that declares another refuses, rather than attaching one set's
/// members to another set's name on the consent card.
///
/// A set that could not be *resolved* never reaches here: that failure fails the
/// whole request at the caller (`atproto-pds-full.md:331`), because the reason
/// describes our outbound network and must not be echoed to whoever chose the
/// NSID.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn finish_par_request(pending: PendingPar, records: Vec<Vec<u8>>) -> ParVerdict {
    if records.len() != pending.includes.len() {
        return ParVerdict::deny(
            OAUTH_ERR_SERVER_ERROR,
            "the permission-set documents do not correspond to the sets this \
             request named",
        );
    }

    let PendingPar {
        request: accepted,
        includes,
    } = pending;
    let mut scopes = accepted.scopes;
    let mut sets = Vec::with_capacity(records.len());
    for (include, record) in includes.into_iter().zip(records) {
        let mut set = match expand_permission_set(include, record) {
            Expansion::Expanded { set } => set,
            // The document was fetched and verified but cannot be used — a
            // substituted `id`, a missing `defs.main`, more members than the
            // per-set cap. The reason is the module's own and safe to echo: it
            // describes the third party's published document, not our network.
            Expansion::Refused { reason } => {
                return ParVerdict::deny(OAUTH_ERR_INVALID_SCOPE, reason);
            }
        };

        // A member the matrix grants nothing under reaches neither the token
        // nor the card. Dropping it is the *Scope model* bullet's rule applied
        // one level down — a card row that grants nothing is the same lie
        // whether the client asked for it directly or a third party's document
        // did — and it is recorded rather than silently removed.
        let mut granting = Vec::with_capacity(set.members.len());
        let mut dropped = Vec::new();
        for (index, member) in set.members.iter().enumerate() {
            if scope_grants_something(member) {
                granting.push(member.clone());
            } else {
                dropped.push(IgnoredMember {
                    index: index as u32,
                    kind: String::new(),
                    reason: IgnoreReason::NotGrantable,
                    detail: member.clone(),
                });
            }
        }
        set.ignored.extend(dropped);
        // `atproto-pds-full.md:330`: an include that grants nothing refuses at
        // PAR exactly as a dead scalar scope does — one refusal for both causes
        // (nothing survived the document's own ignore rules, or nothing
        // survived the matrix), because the client's remedy is the same and the
        // difference is in `ignored` for whoever debugs it.
        if granting.is_empty() {
            return ParVerdict::deny(
                OAUTH_ERR_INVALID_SCOPE,
                format!(
                    "permission set `{}` grants nothing on this authorization server",
                    set.nsid
                ),
            );
        }
        for member in &granting {
            if !scopes.iter().any(|s| s == member) {
                scopes.push(member.clone());
            }
        }
        set.members = granting;
        sets.push(set);
    }

    // The grant-level caps, over everything the token would carry.
    if let GrantExpansion::Refused { reason } =
        check_grant_expansion(sets.len() as u32, scopes.clone())
    {
        return ParVerdict::deny(
            OAUTH_ERR_INVALID_SCOPE,
            format!("this authorization request {reason}"),
        );
    }
    // And over what the *card and the grant row* carry, which the check above
    // does not see: it measures the deduped union, while the set payload
    // repeats every member under its own set. See `check_sets_payload`.
    if let GrantExpansion::Refused { reason } = check_sets_payload(&sets) {
        return ParVerdict::deny(
            OAUTH_ERR_INVALID_SCOPE,
            format!("this authorization request {reason}"),
        );
    }

    ParVerdict::Accept {
        request: AcceptedParRequest {
            scopes,
            sets,
            ..accepted
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::oauth_client::{
        ClientIdPlan, ClientResolution, parse_client_metadata, plan_client_id,
    };

    const CLIENT_ID: &str = "https://app.example.com/client.json";
    const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    fn web_client(scope: &str) -> ResolvedClient {
        let body = format!(
            r#"{{
                "client_id": "{CLIENT_ID}",
                "client_name": "Example App",
                "redirect_uris": ["https://app.example.com/cb"],
                "grant_types": ["authorization_code", "refresh_token"],
                "response_types": ["code"],
                "scope": "{scope}",
                "dpop_bound_access_tokens": true,
                "application_type": "web",
                "token_endpoint_auth_method": "none"
            }}"#
        );
        match parse_client_metadata(CLIENT_ID.to_string(), body) {
            ClientResolution::Resolved { client } => client,
            other => panic!("fixture client must resolve, got {other:?}"),
        }
    }

    fn good_request() -> ParRequest {
        ParRequest {
            client_id: CLIENT_ID.to_string(),
            response_type: "code".to_string(),
            redirect_uri: "https://app.example.com/cb".to_string(),
            scope: "atproto transition:generic".to_string(),
            state: "opaque-csrf".to_string(),
            code_challenge: CHALLENGE.to_string(),
            code_challenge_method: "S256".to_string(),
            login_hint: Some("alice.example.com".to_string()),
            nonce: None,
        }
    }

    /// A request naming no permission sets is decided by the plan step alone,
    /// and this helper asserts that: a fixture that grew an `include:` would
    /// come back `Resolve` and fail here rather than quietly skipping the
    /// expansion half.
    fn accept(request: ParRequest, client: &ResolvedClient) -> AcceptedParRequest {
        match plan_par_request(request, client.clone()) {
            ParPlan::Accept { request } => request,
            other => panic!("expected acceptance, got {other:?}"),
        }
    }

    fn deny(request: ParRequest, client: &ResolvedClient) -> (String, String) {
        match plan_par_request(request, client.clone()) {
            ParPlan::Deny { error, description } => (error, description),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    /// The headline journey: a well-formed request against a well-formed
    /// client is accepted, and what survives into storage is exactly the
    /// consent/token slices' inputs — with the gate-only parameters gone.
    #[test]
    fn a_well_formed_request_is_accepted_and_keeps_only_what_the_flow_needs() {
        let client = web_client("atproto transition:generic");
        let accepted = accept(good_request(), &client);
        assert_eq!(accepted.client_id, CLIENT_ID);
        assert_eq!(accepted.redirect_uri, "https://app.example.com/cb");
        assert_eq!(
            accepted.scopes,
            vec!["atproto".to_string(), "transition:generic".to_string()]
        );
        assert_eq!(accepted.state, "opaque-csrf");
        assert_eq!(accepted.code_challenge, CHALLENGE);
        assert_eq!(accepted.login_hint.as_deref(), Some("alice.example.com"));
    }

    /// The read arm is form-gated (`authorization-server.md` § Scope grammar
    /// → *The read arm*): a public (device) client may ask for it; a
    /// confidential (remote) client is `invalid_scope`, for the qualified
    /// string and the bare plane and verb alike, and on the redirect-less
    /// start as on PAR.
    #[test]
    fn a_folder_read_is_refused_to_a_confidential_client_bare_or_qualified() {
        let read = "fauna:folder:read:42";
        let bare = "fauna:folder:read";
        let device = web_client(&format!("{read} {bare} fauna:folder:deposit:42"));
        let with = |scope: &str| ParRequest {
            scope: scope.to_string(),
            ..good_request()
        };
        assert_eq!(accept(with(read), &device).scopes, vec![read.to_string()]);

        let mut remote = device.clone();
        remote.confidential = true;
        assert!(remote.is_remote_form() && !device.is_remote_form());
        for s in [read, bare] {
            let (error, description) = deny(with(s), &remote);
            assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "{s}");
            assert!(
                description.contains("user's own device"),
                "{s}: {description}"
            );
            let start = StartRequest {
                client_id: CLIENT_ID.to_string(),
                scope: s.to_string(),
                login_hint: None,
            };
            assert!(
                matches!(
                    plan_start_request(start, remote.clone()),
                    ParPlan::Deny { .. }
                ),
                "{s}"
            );
        }
        // The deposit arm reads nothing and stays open to any form.
        assert_eq!(
            accept(with("fauna:folder:deposit:42"), &remote).scopes,
            vec!["fauna:folder:deposit:42".to_string()]
        );
    }

    /// A `records` wildcard is the client's own publisher's or nothing
    /// (`authorization-server.md` § Scope grammar → *Wildcards*); a single
    /// kind of another publisher is legal at PAR and is the consent's call.
    #[test]
    fn a_records_wildcard_names_only_the_documents_own_host() {
        let own = "fauna:records:rw:ext.app.example.com.*";
        let parent = "fauna:records:rw:ext.example.com.*";
        let foreign_kind = "fauna:records:rw:ext.other.org.thing";
        let client = web_client(&format!("{own} {parent} {foreign_kind}"));
        let with = |scope: &str| ParRequest {
            scope: scope.to_string(),
            ..good_request()
        };
        assert_eq!(accept(with(own), &client).scopes, vec![own.to_string()]);
        assert_eq!(
            accept(with(foreign_kind), &client).scopes,
            vec![foreign_kind.to_string()]
        );
        // Host equality, not a registrable-domain rule: the document at
        // `app.example.com` cannot claim `example.com`'s kinds.
        let (error, description) = deny(with(parent), &client);
        assert_eq!(error, OAUTH_ERR_INVALID_SCOPE);
        assert!(description.contains("another publisher"), "{description}");
    }

    /// A redirect-less start (typed code, quiet push) is held to the SAME scope
    /// decision as PAR — the declared set, grantability, the base scope — and
    /// carries no redirect, state or challenge, because it has none.
    #[test]
    fn a_consent_start_is_held_to_the_same_scope_decision_as_par() {
        let client = web_client("atproto transition:generic");
        let start = |scope: &str| StartRequest {
            client_id: CLIENT_ID.to_string(),
            scope: scope.to_string(),
            login_hint: Some("alice.example.com".to_string()),
        };

        let accepted = match plan_start_request(start("atproto transition:generic"), client.clone())
        {
            ParPlan::Accept { request } => request,
            other => panic!("expected acceptance, got {other:?}"),
        };
        assert_eq!(
            accepted.scopes,
            vec!["atproto".to_string(), "transition:generic".to_string()]
        );
        assert_eq!(accepted.login_hint.as_deref(), Some("alice.example.com"));
        assert!(
            accepted.redirect_uri.is_empty()
                && accepted.state.is_empty()
                && accepted.code_challenge.is_empty(),
            "a start with no browser leg carries none of the three: {accepted:?}"
        );

        for (scope, why) in [
            ("", "scope is required"),
            ("transition:generic", "the base scope is required"),
            (
                "atproto transition:chat.bsky",
                "an undeclared scope is refused",
            ),
        ] {
            match plan_start_request(start(scope), client.clone()) {
                ParPlan::Deny { error, .. } => assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "{why}"),
                other => panic!("{why}: expected a denial, got {other:?}"),
            }
        }

        match plan_start_request(
            StartRequest {
                client_id: "https://other.example/client.json".to_string(),
                ..start("atproto")
            },
            client,
        ) {
            ParPlan::Deny { error, .. } => assert_eq!(error, OAUTH_ERR_INVALID_CLIENT),
            other => panic!("a client mismatch must refuse, got {other:?}"),
        }
    }

    /// Every rejection path, as a table. The point is coverage of the *set* —
    /// a new gate is a new row, and a gate that stops refusing shows up here
    /// rather than in a live flow.
    #[test]
    fn every_rejection_path_refuses_with_its_own_error_code() {
        let client = web_client("atproto transition:generic");
        let cases: Vec<(&str, ParRequest, &str, &str)> = vec![
            (
                "a request naming a different client",
                ParRequest {
                    client_id: "https://other.example.com/c.json".into(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_CLIENT,
                "not the client that was resolved",
            ),
            (
                "an implicit-flow response_type",
                ParRequest {
                    response_type: "token".into(),
                    ..good_request()
                },
                OAUTH_ERR_UNSUPPORTED_RESPONSE_TYPE,
                "response_type",
            ),
            (
                "PKCE downgraded to plain",
                ParRequest {
                    code_challenge_method: "plain".into(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_REQUEST,
                "code_challenge_method",
            ),
            (
                "PKCE absent entirely",
                ParRequest {
                    code_challenge: String::new(),
                    code_challenge_method: String::new(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_REQUEST,
                "code_challenge_method",
            ),
            (
                "a malformed S256 challenge",
                ParRequest {
                    code_challenge: "too-short".into(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_REQUEST,
                "base64url",
            ),
            (
                "no state",
                ParRequest {
                    state: String::new(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_REQUEST,
                "state is required",
            ),
            (
                "a redirect_uri the client never declared",
                ParRequest {
                    redirect_uri: "https://evil.example/cb".into(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_REQUEST,
                "not one this client declared",
            ),
            (
                "no scope",
                ParRequest {
                    scope: String::new(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_SCOPE,
                "scope is required",
            ),
            (
                "a scope set missing the base scope",
                ParRequest {
                    scope: "transition:generic".into(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_SCOPE,
                "base `atproto` scope",
            ),
            (
                "a scope the client never declared",
                ParRequest {
                    scope: "atproto blob:*/*".into(),
                    ..good_request()
                },
                OAUTH_ERR_INVALID_SCOPE,
                "not a scope this client declared",
            ),
        ];
        for (name, request, want_error, want_text) in cases {
            let (error, description) = deny(request, &client);
            assert_eq!(error, want_error, "{name}: description was `{description}`");
            assert!(
                description.contains(want_text),
                "{name}: denied with `{description}`, expected it to mention `{want_text}`"
            );
        }
    }

    /// "Sign in with Fauna" (TP6): a request naming only the OIDC family is a
    /// complete request — no `atproto` required — and its `nonce` survives
    /// into what is stored, verbatim, for the ID token to echo.
    #[test]
    fn an_oidc_only_sign_in_is_accepted_and_keeps_its_nonce() {
        let client = web_client("openid profile email");
        let accepted = accept(
            ParRequest {
                scope: "openid profile email".into(),
                nonce: Some("n-0S6_WzA2Mj".into()),
                ..good_request()
            },
            &client,
        );
        assert_eq!(accepted.scopes, vec!["openid", "profile", "email"]);
        assert_eq!(accepted.nonce.as_deref(), Some("n-0S6_WzA2Mj"));
    }

    /// The OIDC family composes with the ATProto family, and then the ATProto
    /// family's own entry rule still holds.
    #[test]
    fn a_mixed_request_still_needs_the_atproto_base() {
        let client = web_client("atproto openid transition:generic");
        let accepted = accept(
            ParRequest {
                scope: "atproto openid transition:generic".into(),
                ..good_request()
            },
            &client,
        );
        assert_eq!(
            accepted.scopes,
            vec!["atproto", "openid", "transition:generic"]
        );

        let (error, description) = deny(
            ParRequest {
                scope: "openid transition:generic".into(),
                ..good_request()
            },
            &client,
        );
        assert_eq!(error, OAUTH_ERR_INVALID_SCOPE);
        assert!(
            description.contains("base `atproto` scope"),
            "{description}"
        );
    }

    /// A principal's request may name only the Fauna family — its reader is
    /// the nest, so no `atproto` base is asked of it — and a built arm is
    /// accepted (`authorization-server.md` § Scope grammar → *The Fauna
    /// family, exactly*).
    #[test]
    fn a_fauna_only_request_naming_a_built_arm_is_accepted() {
        let client = web_client("fauna:feed:read openid");
        let accepted = accept(
            ParRequest {
                scope: "fauna:feed:read".into(),
                ..good_request()
            },
            &client,
        );
        assert_eq!(accepted.scopes, vec!["fauna:feed:read"]);
    }

    /// The folder plane's qualifier is the user's (`authorization-server.md`
    /// § Scope grammar → *The folder plane's qualifier is the user's*): a
    /// document declaring the bare `fauna:folder:deposit` may request it bare
    /// (the card chooses the folder) or qualified (a re-consent); a document
    /// declaring one folder's string still passes by exact match; every other
    /// arm keeps exact membership, and the bare `fauna:records:rw` — its
    /// qualifier is the client's — stays refused.
    #[test]
    fn a_user_qualified_arm_is_held_to_the_documents_bare_declaration() {
        let bare = crate::fauna_scope::SCOPE_FOLDER_DEPOSIT;
        let qualified = "fauna:folder:deposit:42";
        let with = |scope: &str| ParRequest {
            scope: scope.to_string(),
            ..good_request()
        };

        let declares_bare = web_client(bare);
        assert_eq!(accept(with(bare), &declares_bare).scopes, vec![bare]);
        assert_eq!(
            accept(with(qualified), &declares_bare).scopes,
            vec![qualified]
        );
        // The read arm's qualifier is the user's too: a device client may ask
        // for it bare (its form gate is pinned beside the arm).
        let read = crate::fauna_scope::SCOPE_FOLDER_READ;
        let declares_both = web_client(&format!("{bare} {read}"));
        assert_eq!(
            accept(with(&format!("{bare} {read}")), &declares_both).scopes,
            vec![bare, read]
        );
        assert_eq!(
            accept(with("fauna:folder:read:42"), &declares_both).scopes,
            vec!["fauna:folder:read:42"]
        );
        let declares_qualified = web_client(qualified);
        assert_eq!(
            accept(with(qualified), &declares_qualified).scopes,
            vec![qualified]
        );

        // Narrowing never widens: a qualified declaration does not admit the
        // bare request, and an undeclared bare request is refused.
        for (requested, client) in [
            (bare, &declares_qualified),
            (bare, &web_client("fauna:feed:read")),
            (qualified, &web_client("fauna:feed:read")),
        ] {
            let (error, description) = deny(with(requested), client);
            assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "{requested}");
            assert!(
                description.contains("declared"),
                "{requested}: {description}"
            );
        }

        // The records arm's qualifier is the client's: bare stays refused even
        // when declared.
        let records = crate::fauna_scope::SCOPE_RECORDS_RW;
        let (error, description) = deny(with(records), &web_client(records));
        assert_eq!(error, OAUTH_ERR_INVALID_SCOPE);
        assert!(description.contains("can grant"), "{description}");
    }

    /// Any `fauna:` string matching no built arm is `invalid_scope` — a plane
    /// the goal doc lists and no slice has built included — even when the
    /// client declared it.
    #[test]
    fn a_fauna_scope_matching_no_arm_is_refused() {
        for scope in [
            "fauna:folder:read:abc",
            "fauna:records:rw:fauna.state.notes",
            "fauna:post:write",
            "fauna:feed:read:home",
            "fauna:feed:write",
            "fauna:Feed:read",
        ] {
            let (error, description) = deny(
                ParRequest {
                    scope: scope.into(),
                    ..good_request()
                },
                &web_client(scope),
            );
            assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "for {scope}");
            assert!(description.contains("can grant"), "{scope}: {description}");
        }
    }

    /// `profile` and `email` without `openid` grant nothing — no surface
    /// releases their claims outside a sign-in — so PAR refuses them rather
    /// than putting an inert row on the card.
    #[test]
    fn profile_or_email_without_openid_is_refused() {
        for scope in ["profile", "email", "atproto email"] {
            let (error, description) = deny(
                ParRequest {
                    scope: scope.into(),
                    ..good_request()
                },
                &web_client(scope),
            );
            assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "for {scope}");
            assert!(
                description.contains("together with `openid`"),
                "{description}"
            );
        }
    }

    /// The nonce is an anonymous caller's value, stored and echoed into a
    /// signed token, so its size is bounded — and an empty one is no nonce.
    #[test]
    fn an_empty_or_oversized_nonce_is_refused() {
        let client = web_client("openid");
        for nonce in [String::new(), "n".repeat(OIDC_NONCE_MAX_LEN + 1)] {
            let (error, _) = deny(
                ParRequest {
                    scope: "openid".into(),
                    nonce: Some(nonce),
                    ..good_request()
                },
                &client,
            );
            assert_eq!(error, OAUTH_ERR_INVALID_REQUEST);
        }
        let at_the_bound = accept(
            ParRequest {
                scope: "openid".into(),
                nonce: Some("n".repeat(OIDC_NONCE_MAX_LEN)),
                ..good_request()
            },
            &client,
        );
        assert_eq!(
            at_the_bound.nonce.map(|n| n.len()),
            Some(OIDC_NONCE_MAX_LEN)
        );
    }

    /// **The slice's headline honesty property, and the second half of
    /// `every_advertised_scope_grants_something`.** A client may declare a
    /// scope the matrix denies everywhere — nothing stops it publishing one —
    /// but this server must not accept a request for it. Accepting would put
    /// the scope on the consent screen and in the grant, and every call under
    /// it would still deny.
    ///
    /// A collection-narrowed `repo:` scope is the live case: real grammar, a
    /// client would reasonably ask for it, and it is a *deferred* gap here.
    #[test]
    fn a_scope_the_matrix_grants_nothing_under_refuses_even_when_the_client_declared_it() {
        for ungrantable in [
            "repo:app.bsky.feed.post",
            "transition:email",
            "account:email",
            "identity:handle",
        ] {
            let client = web_client(&format!("atproto {ungrantable}"));
            let (error, description) = deny(
                ParRequest {
                    scope: format!("atproto {ungrantable}"),
                    ..good_request()
                },
                &client,
            );
            assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "for {ungrantable}");
            assert!(
                description.contains("this authorization server can grant"),
                "`{ungrantable}` denied with `{description}` — expected the \
                 grantability refusal, not the declaration one"
            );
        }
    }

    /// The mirror of the above: a scope the client declared **and** the matrix
    /// grants under is accepted, including the narrowed `rpc:` shape whose
    /// probe is derived from the scope itself.
    #[test]
    fn a_declared_and_grantable_scope_is_accepted_including_a_narrowed_rpc() {
        let narrowed = "rpc:app.bsky.feed.getTimeline?aud=did:web:api.bsky.app%23bsky_appview";
        let client = web_client(&format!("atproto {narrowed}"));
        let accepted = accept(
            ParRequest {
                scope: format!("atproto {narrowed}"),
                ..good_request()
            },
            &client,
        );
        assert_eq!(
            accepted.scopes,
            vec!["atproto".to_string(), narrowed.to_string()]
        );
    }

    /// The loopback development client goes through the same validation, and
    /// its one relaxation shows up exactly where it should: a request from an
    /// ephemeral dev-server port matches a declared portless redirect.
    #[test]
    fn the_loopback_client_validates_the_same_way_but_ignores_the_redirect_port() {
        // The loopback client_id is the WHOLE URL, query included — that string
        // is the identity, and the request must name it verbatim.
        let loopback_id = "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb";
        let ClientIdPlan::Resolved { client } = plan_client_id(loopback_id.to_string()) else {
            panic!("the loopback client must resolve");
        };
        let request = ParRequest {
            client_id: loopback_id.to_string(),
            redirect_uri: "http://127.0.0.1:5173/cb".to_string(),
            scope: "atproto".to_string(),
            ..good_request()
        };
        assert_eq!(
            accept(request.clone(), &client).redirect_uri,
            "http://127.0.0.1:5173/cb"
        );

        // …and the relaxation is only the port. A different path still refuses.
        let (error, _) = deny(
            ParRequest {
                redirect_uri: "http://127.0.0.1:5173/other".to_string(),
                ..request
            },
            &client,
        );
        assert_eq!(error, OAUTH_ERR_INVALID_REQUEST);
    }

    // ── Permission sets: the plan/finish pair ────────────────────────────────

    const SET_NSID: &str = "com.example.calendar.appPerms";
    const AUD: &str = "did:web:svc.example";
    /// A member the matrix does grant something under, and the lxm that
    /// produces it.
    const GRANTABLE_LXM: &str = "com.example.calendar.getEvents";
    const GRANTABLE: &str = "rpc:com.example.calendar.getEvents?aud=did:web:svc.example";
    /// A member the matrix grants nothing under, and the reason it does not is
    /// the property the whole design leans on: `no_granular_scope_reaches`
    /// gates `SessionLifecycle` above every granular arm, so a set naming a
    /// session verb gets nothing over it no matter what its document says.
    const SESSION_LIFECYCLE_LXM: &str = "com.atproto.server.deleteSession";
    const SESSION_LIFECYCLE: &str = "rpc:com.atproto.server.deleteSession?aud=did:web:svc.example";

    fn include_scope(nsid: &str) -> String {
        format!("include:{nsid}")
    }

    /// A published permission-set document, dag-cbor encoded exactly as the
    /// verified record arrives from the resolution chain. The tests drive real
    /// documents rather than hand-built expansions, because the expansion is
    /// what `finish_par_request` performs — a fixture that skipped it would be
    /// asserting against this module's own shortcut instead of a set an
    /// authority could actually publish.
    fn document(nsid: &str, permissions: serde_json::Value) -> Vec<u8> {
        let ipld: ipld_core::ipld::Ipld = serde_json::from_value(serde_json::json!({
            "lexicon": 1,
            "id": nsid,
            "defs": {
                "main": {
                    "type": "permission-set",
                    "title": "Calendar access",
                    "details": "Read and write your calendar events.",
                    "permissions": permissions,
                }
            }
        }))
        .expect("fixture is valid IPLD");
        serde_ipld_dagcbor::to_vec(&ipld).expect("fixture encodes")
    }

    /// A document whose members expand to exactly `lxms`, each carrying the
    /// audience the set names itself.
    fn rpc_document(nsid: &str, lxms: &[&str]) -> Vec<u8> {
        let members: Vec<serde_json::Value> = lxms
            .iter()
            .map(|lxm| serde_json::json!({"resource": "rpc", "lxm": lxm, "aud": AUD}))
            .collect();
        document(nsid, serde_json::Value::Array(members))
    }

    /// Plan the given scope string against a client that declared exactly it.
    fn plan(scope: &str) -> ParPlan {
        let client = web_client(scope);
        plan_par_request(
            ParRequest {
                scope: scope.to_string(),
                ..good_request()
            },
            client,
        )
    }

    fn pending(scope: &str) -> PendingPar {
        match plan(scope) {
            ParPlan::Resolve { pending } => pending,
            other => panic!("expected a resolve plan, got {other:?}"),
        }
    }

    /// The headline: an `include:` scope does not decide the request by itself
    /// — it asks the caller to resolve, carrying the parsed NSID and audience,
    /// and the include string itself is NOT among the direct scopes (a bare
    /// `include:` is not something D8 can act on).
    #[test]
    fn a_request_naming_a_set_asks_the_caller_to_resolve_it() {
        let scope = format!(
            "atproto {}?aud=did:web:svc.example",
            include_scope(SET_NSID)
        );
        let pending = pending(&scope);
        assert_eq!(pending.request.scopes, vec!["atproto".to_string()]);
        assert!(
            pending.request.sets.is_empty(),
            "sets arrive at finish time"
        );
        assert_eq!(pending.includes.len(), 1);
        assert_eq!(pending.includes[0].nsid, SET_NSID);
        assert_eq!(
            pending.includes[0].aud.as_deref(),
            Some("did:web:svc.example")
        );
    }

    /// Every refusal the pure half can reach happens before the caller is told
    /// to fetch anything — the property `atproto-pds-full.md:331` and `:332`
    /// both rest on. A `Resolve` verdict for any of these would mean a DNS
    /// query and an HTTPS request bought by a request we had already refused.
    #[test]
    fn every_include_refusal_the_pure_half_can_reach_happens_before_any_fetch() {
        let nine = (0..9)
            .map(|i| format!("include:com.example.s{i}.perms"))
            .collect::<Vec<_>>()
            .join(" ");
        let cases: Vec<(&str, String, &str)> = vec![
            (
                "a malformed NSID",
                format!("atproto {}", include_scope("not an nsid")),
                "is not a valid NSID",
            ),
            (
                "an unread query parameter the client believes it set",
                format!("atproto {}?scope=all", include_scope(SET_NSID)),
                "unsupported parameter",
            ),
            (
                "two audiences on one include",
                format!(
                    "atproto {}?aud=did:web:a&aud=did:web:b",
                    include_scope(SET_NSID)
                ),
                "more than one `aud`",
            ),
            (
                "more sets than the fan-out cap allows",
                format!("atproto {nine}"),
                "over the 8-set limit",
            ),
        ];
        for (name, scope, expect) in cases {
            match plan(&scope) {
                ParPlan::Deny { error, description } => {
                    assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "{name}");
                    assert!(
                        description.contains(expect),
                        "{name}: expected {expect:?} in {description:?}"
                    );
                }
                other => panic!("{name} must refuse before any resolution, got {other:?}"),
            }
        }
    }

    /// An `include:` is held to the client's own declared set like every other
    /// scope — the metadata document is the client's public commitment, and a
    /// set it never declared is one its users could not have audited.
    #[test]
    fn an_include_the_client_never_declared_refuses_like_any_other_scope() {
        let client = web_client("atproto");
        let verdict = plan_par_request(
            ParRequest {
                scope: format!("atproto {}", include_scope(SET_NSID)),
                ..good_request()
            },
            client,
        );
        match verdict {
            ParPlan::Deny { error, description } => {
                assert_eq!(error, OAUTH_ERR_INVALID_SCOPE);
                assert!(description.contains("declared"), "{description}");
            }
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    /// The finish half's headline: the expansion becomes ordinary effective
    /// scopes, and the set's identity survives beside them for the card.
    #[test]
    fn finishing_merges_the_expansion_and_keeps_the_sets_identity() {
        let scope = format!("atproto {}", include_scope(SET_NSID));
        let verdict = finish_par_request(
            pending(&scope),
            vec![rpc_document(SET_NSID, &[GRANTABLE_LXM])],
        );
        let ParVerdict::Accept { request } = verdict else {
            panic!("expected acceptance, got {verdict:?}");
        };
        assert_eq!(
            request.scopes,
            vec!["atproto".to_string(), GRANTABLE.to_string()],
            "the token carries the frozen expansion, not the include"
        );
        assert_eq!(request.sets.len(), 1);
        assert_eq!(request.sets[0].nsid, SET_NSID);
        assert_eq!(request.sets[0].title.as_deref(), Some("Calendar access"));
    }

    /// A member the matrix grants nothing under reaches neither the token nor
    /// the card — and is recorded rather than silently dropped, so a set author
    /// whose document says more than this server will honour can find out why.
    ///
    /// This is also the **D8 pin the LEAD asks for**, and it needs a set inside
    /// `com.atproto.server`'s own namespace to be worth anything: an ordinary
    /// third-party set naming a session verb is already stopped by the expander's
    /// hierarchy constraint, so a test using one would pass without the matrix
    /// ever being consulted. Here the authority IS the namespace, the member is
    /// perfectly in-bounds for its set, and the only thing standing between it
    /// and the grant is `no_granular_scope_reaches`.
    #[test]
    fn a_member_the_matrix_grants_nothing_under_is_dropped_and_recorded() {
        let nsid = "com.atproto.server.appPerms";
        let scope = format!("atproto {}", include_scope(nsid));
        let verdict = finish_par_request(
            pending(&scope),
            vec![rpc_document(
                nsid,
                &["com.atproto.server.getSession", SESSION_LIFECYCLE_LXM],
            )],
        );
        let ParVerdict::Accept { request } = verdict else {
            panic!("expected acceptance, got {verdict:?}");
        };
        assert!(
            !request.scopes.iter().any(|s| s == SESSION_LIFECYCLE),
            "a session-lifecycle member must not reach the grant: {:?}",
            request.scopes
        );
        assert_eq!(
            request.sets[0].members,
            vec![format!("rpc:com.atproto.server.getSession?aud={AUD}")],
            "only the member the matrix answers for survives"
        );
        let dropped: Vec<_> = request.sets[0]
            .ignored
            .iter()
            .filter(|i| i.reason == IgnoreReason::NotGrantable)
            .collect();
        assert_eq!(dropped.len(), 1, "the drop must be data, not silence");
        assert_eq!(dropped[0].detail, SESSION_LIFECYCLE);
    }

    /// `atproto-pds-full.md:330` — an include that grants nothing refuses at
    /// PAR exactly as a dead scalar scope does, and both causes are one
    /// refusal: nothing survived the document's ignore rules, or nothing
    /// survived the matrix.
    #[test]
    fn a_set_that_grants_nothing_refuses_the_whole_request() {
        let scope = format!("atproto {}", include_scope(SET_NSID));
        for (name, doc) in [
            (
                "empty after the document's own ignore rules",
                document(
                    SET_NSID,
                    serde_json::json!([{"resource": "blob", "accept": ["image/*"]}]),
                ),
            ),
            (
                "every member denied by the matrix",
                rpc_document(SET_NSID, &[SESSION_LIFECYCLE_LXM]),
            ),
        ] {
            let verdict = finish_par_request(pending(&scope), vec![doc]);
            match verdict {
                ParVerdict::Deny { error, description } => {
                    assert_eq!(error, OAUTH_ERR_INVALID_SCOPE, "{name}");
                    assert!(description.contains(SET_NSID), "{name}: {description}");
                    assert!(
                        description.contains("grants nothing"),
                        "{name}: {description}"
                    );
                }
                other => panic!("{name} must refuse, got {other:?}"),
            }
        }
    }

    /// A scope the client requested directly *and* a set expanded to costs the
    /// token one entry. The byte budget is real, and a duplicate buys nothing.
    ///
    /// (Two different sets cannot name the same scope — the hierarchy constraint
    /// confines each to its own namespace — so the collision that can actually
    /// happen is exactly this one.)
    #[test]
    fn a_scope_two_sources_name_appears_once_in_the_grant() {
        let scope = format!("atproto {GRANTABLE} {}", include_scope(SET_NSID));
        let verdict = finish_par_request(
            pending(&scope),
            vec![rpc_document(SET_NSID, &[GRANTABLE_LXM])],
        );
        let ParVerdict::Accept { request } = verdict else {
            panic!("expected acceptance, got {verdict:?}");
        };
        assert_eq!(
            request.scopes,
            vec!["atproto".to_string(), GRANTABLE.to_string()]
        );
        assert_eq!(request.sets.len(), 1);
    }

    /// The pairing between what was asked for and what came back is asserted,
    /// not assumed. A misaligned pair would attach one set's members to another
    /// set's name on the consent card — the user would approve a card that
    /// describes a grant nobody computed.
    #[test]
    fn an_expansion_that_does_not_correspond_to_the_request_refuses() {
        let scope = format!("atproto {}", include_scope(SET_NSID));
        // A wrong-length answer is a broken seam: nothing about the request
        // caused it, so it refuses as `server_error` rather than blaming the
        // client for a correspondence only we could get wrong.
        match finish_par_request(pending(&scope), vec![]) {
            ParVerdict::Deny { error, .. } => assert_eq!(error, OAUTH_ERR_SERVER_ERROR),
            other => panic!("a wrong-length answer must refuse, got {other:?}"),
        }
        // A right-length answer carrying ANOTHER set's document is caught by
        // the expander's own `id` check — the substitution that would otherwise
        // put one set's members under another set's name on the consent card.
        match finish_par_request(
            pending(&scope),
            vec![rpc_document("com.example.other.appPerms", &[GRANTABLE_LXM])],
        ) {
            ParVerdict::Deny { error, description } => {
                assert_eq!(error, OAUTH_ERR_INVALID_SCOPE);
                assert!(description.contains("declares `id`"), "{description}");
            }
            other => panic!("a substituted document must refuse, got {other:?}"),
        }
    }

    /// The grant-level caps bound what the access token can carry, and they are
    /// checked over the WHOLE effective scope list — direct scopes included,
    /// since the token does not care which source a scope came from.
    #[test]
    fn an_expansion_past_the_grant_caps_refuses() {
        let scope = format!("atproto {}", include_scope(SET_NSID));
        let lxms: Vec<String> = (0..200)
            .map(|i| format!("com.example.calendar.m{i}"))
            .collect();
        let refs: Vec<&str> = lxms.iter().map(String::as_str).collect();
        match finish_par_request(pending(&scope), vec![rpc_document(SET_NSID, &refs)]) {
            ParVerdict::Deny { error, description } => {
                assert_eq!(error, OAUTH_ERR_INVALID_SCOPE);
                assert!(description.contains("limit"), "{description}");
            }
            other => panic!("an over-cap expansion must refuse, got {other:?}"),
        }
    }
}
