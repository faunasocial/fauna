//! F4 — **client identity**: turning an OAuth `client_id` URL into a validated
//! client-metadata document (`atproto-pds-full.md` § F4 detail, *Client
//! metadata resolution*).
//!
//! ATProto OAuth has no client registration. The `client_id` **is** a URL, and
//! the document it serves is the client's public, self-published commitment:
//! which redirect URIs it will accept, which scopes it may ask for, whether it
//! is confidential. Resolving that document is therefore the act of learning
//! who is asking — and every input to it is attacker-chosen.
//!
//! # The split with Go, and why the URL is never parsed twice
//!
//! Go performs the fetch, because a fetch is I/O; this module is pure. But the
//! split is sharper than that, and it is the same one [`crate::fetch_guard`]
//! documents at length: **Go parses the URL, because Go dials it.** A second
//! parse here could disagree about which host the URL names, which is the
//! classic SSRF bypass where the check inspects host A and the connection
//! reaches host B.
//!
//! So [`plan_client_id`] does not parse a URL. It classifies one:
//!
//! * `http://localhost[/][?…]` → the **loopback dev client**, whose document
//!   is synthesized here from the query string with no fetch at all.
//! * `https://…` → a [`ClientIdPlan::Fetch`] carrying the client_id back
//!   verbatim, for Go to parse, resolve, run past [`crate::fetch_guard`], and
//!   GET. Everything this module adds on top is a *tightening* expressed as a
//!   substring test (no fragment, no userinfo, no port) — never a substitute
//!   for the guard's own verdict.
//! * anything else → deny.
//!
//! Go then hands the response body to [`parse_client_metadata`], which
//! validates it and produces the [`ResolvedClient`] the consent page renders
//! and the PAR check (`crate::oauth_par`) evaluates against.
//!
//! # Closed world
//!
//! Default-deny throughout, like [`crate::authz`] and [`crate::fetch_guard`].
//! A missing required member, an unparseable body, a `client_id` field that
//! does not match the URL it came from — every one of them refuses. Unknown
//! *members* are ignored rather than refused: the document format grows, and
//! refusing a client for carrying a field we have not learned would break it
//! for a reason that is ours.

use serde::{Deserialize, Serialize};

/// The base scope every ATProto authorization request must carry. Named from
/// [`crate::authz`] so this module holds one constant, not a copy.
use crate::authz::{SCOPE_ATPROTO_BASE, lacks_atproto_base};

// ── OAuth error codes (RFC 6749 §5.2 / RFC 9126 §2.3) ────────────────────────

/// The request itself is malformed — a missing member, a bad redirect URI.
pub const OAUTH_ERR_INVALID_REQUEST: &str = "invalid_request";
/// The client could not be identified: an unusable `client_id`, an
/// unresolvable document, a document that does not describe this client.
pub const OAUTH_ERR_INVALID_CLIENT: &str = "invalid_client";
/// A requested scope is not one this authorization server can grant.
pub const OAUTH_ERR_INVALID_SCOPE: &str = "invalid_scope";
/// `response_type` was something other than `code`.
pub const OAUTH_ERR_UNSUPPORTED_RESPONSE_TYPE: &str = "unsupported_response_type";
/// This server broke, not the request. Reserved for states no caller can cause
/// — a seam that answered with something structurally impossible — so that a
/// client developer reading it knows to report it rather than to change their
/// request. Go maps it to 500 (`parErrorStatus`).
pub const OAUTH_ERR_SERVER_ERROR: &str = "server_error";

// ── The loopback development client (`atproto.com/specs/oauth`) ──────────────

/// The one `client_id` that resolves without a fetch.
///
/// The spec is exact and narrow here: `http://localhost`, **no port**, empty
/// path, and IP-literal hosts (`http://127.0.0.1`) are *not* accepted as a
/// client_id — only as a redirect target. Both narrowings are load-bearing:
/// a port or an IP host would give the same development identity more than
/// one spelling, and a client identity with several spellings is one the
/// consent screen cannot describe.
pub const LOOPBACK_CLIENT_ID: &str = "http://localhost";

/// The name the consent screen shows for the loopback client.
///
/// Chosen by **this authorization server**, not by the client — the loopback
/// document carries no `client_name` member at all, which is exactly the
/// property § F4 detail asks for everywhere ("never the client's
/// self-asserted string alone") and gets here for free.
pub const LOOPBACK_CLIENT_NAME: &str = "Development client";

/// Redirect URIs the loopback client gets when it declares none.
pub const LOOPBACK_DEFAULT_REDIRECT_URIS: &[&str] = &["http://127.0.0.1/", "http://[::1]/"];

/// The two hosts a loopback redirect URI may name. Anything else — a public
/// host, a different private address, a custom scheme — is refused.
///
/// **This is the security boundary of the whole carve-out.** `http://localhost`
/// is an identity nobody owns and anybody may claim, so if it could also name
/// an arbitrary redirect target, an attacker would hold a consent-screen
/// identity that redirects the authorization response to a host they control.
/// Keeping the target on the user's own loopback interface is what makes an
/// unowned identity safe to offer: the code goes to the machine the user is
/// sitting at, or nowhere.
const LOOPBACK_REDIRECT_HOSTS: &[&str] = &["127.0.0.1", "[::1]"];

// ── Types ────────────────────────────────────────────────────────────────────

/// A client identity, as **resolved** — never as asserted.
///
/// This is what the consent ceremony renders and what `crate::oauth_par`
/// evaluates a request against. It deliberately carries the client's *display*
/// members alongside `client_id`, because § F4 detail requires the consent page
/// to show the resolved name and logo **beside the requesting origin**.
///
/// ⚠ The origin ANCHORS the display members; it is not itself unforgeable —
/// state the claim precisely or the next reader inherits a false
/// premise. For an **https** client the origin is pinned by
/// the fetch: the document must live at that URL and name it back, so a client
/// can only present an origin it controls. The **loopback** identity is the
/// deliberate exception on BOTH halves: `http://localhost` is claimable by
/// anybody by design, and its spelling includes a free-form query whose bytes
/// once carried row structure into the consent card. What keeps the anchor
/// honest there is [`LOOPBACK_REDIRECT_HOSTS`] (the response can only reach
/// the user's own machine) plus [`plan_client_id`]'s control-character refusal
/// (the spelling cannot forge the card that renders it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ResolvedClient {
    /// The `client_id` URL, verbatim as the request presented it. The
    /// requesting origin the consent screen anchors on.
    pub client_id: String,
    /// Display name, as resolved. `None` for a document that omits it — the
    /// consent screen then has only the origin, which is the honest state.
    pub client_name: Option<String>,
    /// Homepage. Display only.
    pub client_uri: Option<String>,
    /// Logo. Display only — **and deliberately not fetched by this server.**
    /// Whether and how the approving app loads it is the consent slice's
    /// decision, because loading an attacker-named URL inside the user's app
    /// discloses the user's address to whoever published the document.
    pub logo_uri: Option<String>,
    /// Terms of service. Display only.
    pub tos_uri: Option<String>,
    /// Privacy policy. Display only.
    pub policy_uri: Option<String>,
    /// Every redirect URI this client may use. Non-empty by construction —
    /// a document declaring none is refused.
    pub redirect_uris: Vec<String>,
    /// The scopes the client declares it might request, split from the
    /// document's space-delimited `scope` member. A PAR asking for anything
    /// outside this set is refused: the document is the client's public
    /// commitment, and a grant wider than it would be one the client's own
    /// users could not have audited.
    pub declared_scopes: Vec<String>,
    /// `true` when the document declares `token_endpoint_auth_method:
    /// private_key_jwt` — a confidential client, which must authenticate at
    /// the token endpoint.
    ///
    /// ⚠ **This member is the single owner of "is this client confidential".**
    /// The presence of [`Self::jwks`] never implies it and must never be read
    /// as implying it: a public client is free to publish keys for reasons of
    /// its own, and inferring an authentication method from key material is how
    /// a client ends up held to a contract its document never made.
    pub confidential: bool,
    /// The client's ES256 signing keys, as its metadata document declares them
    /// — populated **only for a confidential client**, because only a
    /// confidential client's assertions are ever verified against them.
    ///
    /// Resolved eagerly and completely: a document declaring `jwks_uri` has
    /// already been followed by the time this client exists, so authenticating
    /// an assertion is a pure function over data in hand and performs no
    /// network I/O at all. See [`attach_client_jwks`] for why that ordering was
    /// chosen over fetching lazily at assertion time.
    ///
    /// Non-empty whenever [`Self::confidential`] is set — a confidential client
    /// with no usable key is refused at resolution rather than at the first
    /// assertion, where the failure would look like the client's fault.
    pub jwks: Vec<ClientJwk>,
    /// The `jwks_uri` the document declared, if any — carried so the resolving
    /// caller knows there is a second fetch to make, and kept afterwards purely
    /// as the provenance of [`Self::jwks`].
    pub jwks_uri: Option<String>,
    /// `true` for the loopback development client. Two behaviours key off it:
    /// redirect matching ignores the port, and the display name is this
    /// server's, not the client's.
    pub loopback: bool,
    /// The document's `fauna` member — the signed kind manifest, a compact JWS
    /// carried verbatim (`third-party-kinds.md` § The manifest). `None` for a
    /// document without one. Only its *form* is checked here (a string, not a
    /// bare object); the signature, the header and the payload are verified
    /// by the one shared-Rust door, `fauna_protocol::kind_manifest::verify_manifest`,
    /// which the resolving server runs against the document's host before
    /// this client is accepted.
    #[serde(default)]
    pub fauna_manifest: Option<String>,
}

impl ResolvedClient {
    /// Whether a principal minted from this client runs as a server of its
    /// own — the `remote` execution form, by rule 3's derivation
    /// (`third-party.md` § The principal model): a CONFIDENTIAL client, since
    /// only a party that can keep a private key off the user's device can
    /// authenticate with `private_key_jwt`. Read off the client's own
    /// document, never off anything it says about itself. The one predicate
    /// the PAR form gate and the consent's principal mint both call.
    #[must_use]
    pub fn is_remote_form(&self) -> bool {
        self.confidential
    }
}

/// One ES256 verification key from a client's declared key set.
///
/// Deliberately *not* a general JWK: this server verifies exactly one algorithm
/// (ES256 on P-256 — what the AS document advertises and what ATProto mandates),
/// so a key it cannot use is a key it has no reason to carry. Non-EC and
/// non-P-256 entries are **skipped** rather than refused, because a real key set
/// legitimately holds keys for other purposes; a set with no usable key left
/// after skipping is what refuses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Record))]
pub struct ClientJwk {
    /// The key's identifier. `None` for a set that declares a single unnamed
    /// key — legal, and the only case in which an assertion may omit `kid`.
    pub kid: Option<String>,
    /// Affine coordinates, 32 bytes each, big-endian and zero-padded as JWK
    /// requires ([`crate::jws::decode_ec_coordinate`] owns the width rule).
    pub x: Vec<u8>,
    pub y: Vec<u8>,
}

/// What Go must do to resolve a `client_id`.
///
/// Three outcomes rather than "a URL or an error", because the loopback client
/// resolves with **no fetch at all** — folding it into the fetch path would
/// mean either fetching `http://localhost` (which the SSRF guard correctly
/// refuses) or teaching Go an exception, and Go does not hold policy.
// `clippy::large_enum_variant`: the `Resolved` arm carries the whole
// `ResolvedClient` by value because this enum crosses UniFFI, whose enum
// payloads cannot be boxed; a resolution is built once per client and
// cached, never a hot path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ClientIdPlan {
    /// GET this URL through `safefetch` — the address checks are
    /// [`crate::fetch_guard`]'s, not this module's — then hand the body to
    /// [`parse_client_metadata`] with the same `client_id`.
    Fetch { url: String },
    /// Already resolved; no network. The loopback development client.
    Resolved { client: ResolvedClient },
    /// Refuse before any I/O.
    Deny { error: String, description: String },
}

/// The outcome of validating a fetched metadata document.
// `clippy::large_enum_variant`: the `Resolved` arm carries the whole
// `ResolvedClient` by value because this enum crosses UniFFI, whose enum
// payloads cannot be boxed; a resolution is built once per client and
// cached, never a hot path.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "uniffi", derive(uniffi::Enum))]
pub enum ClientResolution {
    Resolved { client: ResolvedClient },
    Deny { error: String, description: String },
}

impl ClientIdPlan {
    fn deny(error: &str, description: &str) -> Self {
        ClientIdPlan::Deny {
            error: error.to_string(),
            description: description.to_string(),
        }
    }
}

impl ClientResolution {
    fn deny(error: &str, description: &str) -> Self {
        ClientResolution::Deny {
            error: error.to_string(),
            description: description.to_string(),
        }
    }
}

// ── Step 1: classify the client_id ───────────────────────────────────────────

/// Classify a `client_id` into the work Go must do for it.
///
/// Pure and network-free. See the module docs for why this classifies rather
/// than parses.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn plan_client_id(client_id: String) -> ClientIdPlan {
    // A URL cannot carry a raw control character — RFC 3986's character set
    // excludes them and WHATWG agrees — and a client_id that smuggled one
    // would forge LINE STRUCTURE in every surface that renders the identity
    // verbatim: the consent card's rows, the browser authorize page, the
    // connected-apps grant registry, logs (a newline
    // inside the loopback client_id's own query painted an attacker-authored
    // "It is asking to:" heading above the real one). One refusal here, ahead
    // of BOTH arms, fences every downstream consumer at once. Deny — never
    // strip: the client_id is the client's self-authenticating identity, so
    // mangling it would make the string a user is told to trust differ from
    // the identity actually authorized. Strip is for text meant to be read;
    // refuse is for text meant to be compared.
    if client_id.chars().any(char::is_control) {
        return ClientIdPlan::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client_id must not contain control characters",
        );
    }

    if let Some(rest) = loopback_query(&client_id) {
        return match synthesize_loopback_client(&client_id, rest) {
            Ok(client) => ClientIdPlan::Resolved { client },
            Err((error, description)) => ClientIdPlan::deny(error, &description),
        };
    }

    let Some(after_scheme) = client_id.strip_prefix("https://") else {
        return ClientIdPlan::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client_id must be an https URL, or the loopback development \
             client http://localhost",
        );
    };

    // A fragment is never sent to the server, so a `client_id` carrying one
    // could never equal the document's own `client_id` member — the mismatch
    // would surface as a confusing document error. Refuse it by name instead.
    if client_id.contains('#') {
        return ClientIdPlan::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client_id must not carry a fragment",
        );
    }

    // The authority is everything before the first `/`, `?` — the segment Go's
    // parser will read as host[:port]. Two tightenings on it, both cheap and
    // both stated as substring facts so they cannot disagree with a parse:
    let authority = after_scheme
        .split(['/', '?'])
        .next()
        .unwrap_or(after_scheme);
    if authority.is_empty() {
        return ClientIdPlan::deny(OAUTH_ERR_INVALID_CLIENT, "client_id names no host");
    }
    // Userinfo. `https://evil.example@good.example/` reads as `good.example`
    // to a correct parser and as `evil.example` to a careless one — including
    // to a human reading a consent screen, which is the reason it matters here
    // rather than only at the guard.
    if authority.contains('@') {
        return ClientIdPlan::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client_id must not carry userinfo",
        );
    }
    // No port, per spec. One host, one spelling, one identity.
    if authority.contains(':') {
        return ClientIdPlan::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client_id must not carry a port number",
        );
    }

    ClientIdPlan::Fetch { url: client_id }
}

/// Is this the loopback client_id, and if so what is its query string?
///
/// Returns `Some("")` for a bare `http://localhost` or `http://localhost/`.
/// Deliberately exact: `http://localhost:8080`, `http://localhost/app` and
/// `http://127.0.0.1` are all *not* the loopback client, and fall through to
/// the https arm, where they are denied by name.
fn loopback_query(client_id: &str) -> Option<&str> {
    let rest = client_id.strip_prefix(LOOPBACK_CLIENT_ID)?;
    // Path must be empty or exactly "/".
    let rest = rest.strip_prefix('/').unwrap_or(rest);
    match rest.strip_prefix('?') {
        Some(query) => Some(query),
        None if rest.is_empty() => Some(""),
        None => None,
    }
}

/// Build the loopback client's virtual metadata document from its query
/// string.
///
/// Every member is fixed by the spec except the two the query may configure:
/// `redirect_uri` (repeatable) and `scope` (single). What this server chooses
/// is only the display name — see [`LOOPBACK_CLIENT_NAME`].
fn synthesize_loopback_client(
    client_id: &str,
    query: &str,
) -> Result<ResolvedClient, (&'static str, String)> {
    let mut redirect_uris: Vec<String> = Vec::new();
    let mut scope: Option<String> = None;

    for pair in query.split('&').filter(|p| !p.is_empty()) {
        let (key, raw) = pair.split_once('=').unwrap_or((pair, ""));
        let value = crate::authz::percent_decode(&raw.replace('+', " "));
        match key {
            "redirect_uri" => redirect_uris.push(value),
            "scope" => {
                if scope.is_some() {
                    return Err((
                        OAUTH_ERR_INVALID_CLIENT,
                        "the loopback client accepts at most one scope parameter".to_string(),
                    ));
                }
                scope = Some(value);
            }
            // Unknown parameters are refused rather than ignored: the loopback
            // client_id IS its document, so an unrecognized parameter means the
            // client believes it configured something this server did not read.
            other => {
                return Err((
                    OAUTH_ERR_INVALID_CLIENT,
                    format!("unsupported loopback client parameter `{other}`"),
                ));
            }
        }
    }

    if redirect_uris.is_empty() {
        redirect_uris = LOOPBACK_DEFAULT_REDIRECT_URIS
            .iter()
            .map(|s| s.to_string())
            .collect();
    } else {
        for uri in &redirect_uris {
            if !is_loopback_redirect_uri(uri) {
                return Err((
                    OAUTH_ERR_INVALID_CLIENT,
                    format!(
                        "loopback client redirect_uri `{uri}` must be an http URL on \
                         127.0.0.1 or [::1]"
                    ),
                ));
            }
        }
    }

    let declared_scopes = split_scope(scope.as_deref().unwrap_or(SCOPE_ATPROTO_BASE));
    if declared_scopes.is_empty() || lacks_atproto_base(&declared_scopes) {
        return Err((
            OAUTH_ERR_INVALID_CLIENT,
            format!(
                "the loopback client's scope must include `{SCOPE_ATPROTO_BASE}` \
                 (unless it names only OIDC and Fauna-family scopes)"
            ),
        ));
    }

    Ok(ResolvedClient {
        client_id: client_id.to_string(),
        // Chosen here, not read from anywhere — see LOOPBACK_CLIENT_NAME.
        client_name: Some(LOOPBACK_CLIENT_NAME.to_string()),
        client_uri: None,
        logo_uri: None,
        tos_uri: None,
        policy_uri: None,
        redirect_uris,
        declared_scopes,
        // `token_endpoint_auth_method: none` — a public native client, so no
        // key set and nothing to authenticate with. The loopback identity is
        // one anybody may claim (see LOOPBACK_CLIENT_ID); letting it declare
        // signing keys would let an attacker claim a *confidential* identity,
        // which is the one thing that carve-out must never offer.
        confidential: false,
        jwks: Vec::new(),
        jwks_uri: None,
        loopback: true,
        fauna_manifest: None,
    })
}

/// Is this a redirect URI the loopback client may declare? See
/// [`LOOPBACK_REDIRECT_HOSTS`] for why the answer is this narrow.
fn is_loopback_redirect_uri(uri: &str) -> bool {
    let Some(after_scheme) = uri.strip_prefix("http://") else {
        return false;
    };
    let authority = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or(after_scheme);
    LOOPBACK_REDIRECT_HOSTS.contains(&strip_port(authority))
}

// ── Step 2: validate the fetched document ────────────────────────────────────

/// The members this server reads out of a client-metadata document.
///
/// Unknown members are ignored (`serde` default) rather than refused: the
/// format grows, and a client carrying a member we have not learned has done
/// nothing wrong.
#[derive(Debug, Deserialize)]
struct ClientMetadataDocument {
    client_id: Option<String>,
    client_name: Option<String>,
    client_uri: Option<String>,
    logo_uri: Option<String>,
    tos_uri: Option<String>,
    policy_uri: Option<String>,
    redirect_uris: Option<Vec<String>>,
    grant_types: Option<Vec<String>>,
    response_types: Option<Vec<String>>,
    scope: Option<String>,
    dpop_bound_access_tokens: Option<bool>,
    token_endpoint_auth_method: Option<String>,
    application_type: Option<String>,
    /// An inline key set. Kept as raw JSON so exactly one parser reads a JWKS,
    /// whether it arrived inline or over the wire from `jwks_uri`.
    jwks: Option<serde_json::Value>,
    jwks_uri: Option<String>,
    /// The Fauna extension (`third-party.md` § The manifest). Raw JSON, so a
    /// bare object — an unsigned manifest — is told apart from the compact
    /// JWS string it must be.
    fauna: Option<serde_json::Value>,
}

/// A JWKS document, inline or fetched. One shape, one parser.
#[derive(Debug, Deserialize)]
struct JwksDocument {
    keys: Option<Vec<JwksKey>>,
}

#[derive(Debug, Deserialize)]
struct JwksKey {
    #[serde(default)]
    kty: Option<String>,
    #[serde(default)]
    crv: Option<String>,
    #[serde(default)]
    kid: Option<String>,
    #[serde(default)]
    x: Option<String>,
    #[serde(default)]
    y: Option<String>,
    #[serde(default)]
    r#use: Option<String>,
    /// The EC private scalar, named so its *presence* is detectable — same
    /// posture as [`crate::dpop`]'s embedded JWK. A client that publishes its
    /// private key at a URL has a key that must be treated as compromised, and
    /// silently verifying against its public half would let it keep
    /// authenticating while we hold a secret we were never meant to see.
    #[serde(default)]
    d: Option<serde_json::Value>,
}

/// Parse a JWKS into the ES256 keys this server can verify with.
///
/// **Skip-don't-refuse, with one exception.** A real key set holds keys for
/// several purposes and algorithms; refusing the whole document because one
/// entry is RSA would break clients that have done nothing wrong. So unusable
/// entries are skipped and an *empty result* is what fails. The exception is a
/// key carrying private material: that is not an entry we cannot use, it is
/// evidence about the client's key hygiene, and it refuses the set outright.
///
/// A malformed *usable-looking* key (EC on P-256 with a short or unparseable
/// coordinate) also refuses rather than being skipped — skipping it would leave
/// the client authenticating under whichever other key happened to parse, which
/// is a silent downgrade of the key set it published.
fn parse_jwks(body: &str) -> Result<Vec<ClientJwk>, String> {
    let doc: JwksDocument = serde_json::from_str(body)
        .map_err(|err| format!("client key set is not valid JSON: {err}"))?;
    let Some(keys) = doc.keys else {
        return Err("client key set has no `keys` array".to_string());
    };

    let mut out = Vec::new();
    for key in keys {
        if key.d.is_some() {
            return Err(
                "client key set carries private key material — refused, not ignored".to_string(),
            );
        }
        // `use: enc` names a key for encryption; verifying a signature with it
        // would be using a key against its published purpose. `sig` and an
        // absent `use` both mean "may sign".
        if key.r#use.as_deref() == Some("enc") {
            continue;
        }
        if key.kty.as_deref() != Some("EC") || key.crv.as_deref() != Some("P-256") {
            continue;
        }
        let x = crate::jws::decode_ec_coordinate(key.x.as_deref(), "x", "client key set")?;
        let y = crate::jws::decode_ec_coordinate(key.y.as_deref(), "y", "client key set")?;
        out.push(ClientJwk {
            kid: non_empty(key.kid),
            x,
            y,
        });
    }

    if out.is_empty() {
        return Err(
            "client key set declares no ES256 (P-256) signing key — the only algorithm \
             this authorization server verifies client assertions with"
                .to_string(),
        );
    }
    // Two keys under one `kid` make key selection ambiguous, and an ambiguity
    // resolved by document order is one an attacker who can append to the set
    // controls. Refuse rather than pick.
    for (i, key) in out.iter().enumerate() {
        if key.kid.is_some() && out[..i].iter().any(|k| k.kid == key.kid) {
            return Err(format!(
                "client key set declares two keys under `kid` `{}` — key selection \
                 must not depend on document order",
                key.kid.as_deref().unwrap_or_default()
            ));
        }
    }
    Ok(out)
}

/// Complete a confidential client whose document declared a `jwks_uri`, from
/// the body fetched at that URI.
///
/// # Why the key set is resolved here rather than at assertion time
///
/// A confidential client's keys are fetched **eagerly**, as part of resolving
/// the client, and the resolved client carries them. The alternative — fetch
/// lazily when an assertion arrives, behind its own cache — was rejected for
/// two reasons that compound:
///
/// * It would put an outbound fetch **inside the authentication decision**, on
///   a path whose whole purpose is to establish who is calling. Resolving
///   eagerly keeps authentication a pure function over data already in hand, so
///   it has no network failure mode, no timing signal and no ordering question.
/// * Lazy resolution needs an answer to "an assertion names a `kid` we have not
///   seen — do we re-fetch?", and *both* answers are bad: yes makes an
///   attacker-chosen `kid` a fetch amplifier, no makes key rotation silently
///   unsupported. Eager resolution never asks it.
///
/// The cost is that a rotated key takes effect only when the client cache entry
/// expires (15 minutes). That is exactly what a key *set* with `kid`s exists to
/// absorb: a client rotating correctly publishes the old and new keys together,
/// and any sane overlap is far longer than the cache TTL.
///
/// Returns the completed client, or a refusal — the same [`ClientResolution`]
/// the document parse returns, so the caller has one shape to handle and one
/// place that turns a refusal into a cached negative.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn attach_client_jwks(client: ResolvedClient, body: String) -> ClientResolution {
    if !client.confidential {
        // Unreachable through the intended caller, which only fetches when the
        // document declared a `jwks_uri` on a confidential client. Refusing
        // rather than ignoring keeps `confidential` the single owner of whether
        // a key set means anything (see `ResolvedClient::confidential`).
        return ClientResolution::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "a key set was resolved for a client that does not authenticate",
        );
    }
    match parse_jwks(&body) {
        Ok(jwks) => ClientResolution::Resolved {
            client: ResolvedClient { jwks, ..client },
        },
        Err(why) => ClientResolution::deny(OAUTH_ERR_INVALID_CLIENT, &why),
    }
}

/// Validate a fetched client-metadata document against the `client_id` it was
/// fetched from.
///
/// `client_id` must be the URL Go actually GET'd — the same string
/// [`plan_client_id`] handed back in [`ClientIdPlan::Fetch`]. The document's
/// own `client_id` member is compared against it by **exact string equality**,
/// per spec: that equality is what makes the URL a self-authenticating
/// identity, because a document served at one URL cannot then claim to be a
/// client living at another.
#[cfg_attr(feature = "uniffi", uniffi::export)]
pub fn parse_client_metadata(client_id: String, body: String) -> ClientResolution {
    let doc: ClientMetadataDocument = match serde_json::from_str(&body) {
        Ok(doc) => doc,
        Err(err) => {
            return ClientResolution::deny(
                OAUTH_ERR_INVALID_CLIENT,
                &format!("client metadata document is not valid JSON: {err}"),
            );
        }
    };

    match doc.client_id.as_deref() {
        Some(declared) if declared == client_id => {}
        Some(_) => {
            return ClientResolution::deny(
                OAUTH_ERR_INVALID_CLIENT,
                "client metadata `client_id` does not match the URL it was fetched from",
            );
        }
        None => {
            return ClientResolution::deny(
                OAUTH_ERR_INVALID_CLIENT,
                "client metadata document has no `client_id`",
            );
        }
    }

    // `dpop_bound_access_tokens` must be present AND true. DPoP is mandatory
    // for every client type in ATProto, so a document that omits or denies it
    // describes a client this server cannot issue a usable token to — refuse
    // now rather than at the token endpoint, where the user has already
    // consented.
    if doc.dpop_bound_access_tokens != Some(true) {
        return ClientResolution::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client metadata must set `dpop_bound_access_tokens` to true",
        );
    }

    let response_types = doc.response_types.unwrap_or_default();
    if !response_types.iter().any(|t| t == "code") {
        return ClientResolution::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client metadata `response_types` must include `code`",
        );
    }

    let grant_types = doc.grant_types.unwrap_or_default();
    if !grant_types.iter().any(|t| t == "authorization_code") {
        return ClientResolution::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client metadata `grant_types` must include `authorization_code`",
        );
    }

    let redirect_uris = doc.redirect_uris.unwrap_or_default();
    if redirect_uris.is_empty() {
        return ClientResolution::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client metadata must declare at least one `redirect_uri`",
        );
    }

    // The ATProto family's entry rule, and no more than it: a client declaring
    // only the OIDC and Fauna families (a "Sign in with Fauna" site, a
    // third-party principal) needs no `atproto`
    // (`authorization-server.md` § Scope grammar). A document declaring
    // nothing at all is still refused — it could ask for nothing.
    let declared_scopes = split_scope(doc.scope.as_deref().unwrap_or_default());
    if declared_scopes.is_empty() || lacks_atproto_base(&declared_scopes) {
        return ClientResolution::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client metadata `scope` must include the base `atproto` scope \
             (unless it declares only OIDC and Fauna-family scopes)",
        );
    }

    // `application_type` defaults to `web`; only `web` and `native` exist. An
    // unrecognized value is a document this server cannot interpret, and the
    // closed-world posture says refuse rather than guess a default that
    // changes how redirect URIs are read.
    let application_type = doc.application_type.as_deref().unwrap_or("web");
    if application_type != "web" && application_type != "native" {
        return ClientResolution::deny(
            OAUTH_ERR_INVALID_CLIENT,
            "client metadata `application_type` must be `web` or `native`",
        );
    }

    // `none` (public, PKCE + DPoP bound) or `private_key_jwt` (confidential).
    // There is no shared secret in this design, so no `client_secret_*` method
    // exists to accept — the AS document advertises exactly these two.
    let confidential = match doc.token_endpoint_auth_method.as_deref().unwrap_or("none") {
        "none" => false,
        "private_key_jwt" => true,
        other => {
            return ClientResolution::deny(
                OAUTH_ERR_INVALID_CLIENT,
                &format!("unsupported `token_endpoint_auth_method` `{other}`"),
            );
        }
    };

    // The key set, and only for a confidential client — `token_endpoint_auth_method`
    // is the single owner of whether this client authenticates, so keys on a
    // public client's document are carried nowhere and mean nothing (see
    // `ResolvedClient::confidential`).
    let jwks_uri = non_empty(doc.jwks_uri);
    let mut jwks = Vec::new();
    if confidential {
        // Exactly one source. Declaring both is not a merge problem — it is a
        // client that has published two answers to "which keys sign my
        // assertions", and picking one would be this server deciding which of
        // the client's own statements to believe.
        match (&doc.jwks, &jwks_uri) {
            (Some(_), Some(_)) => {
                return ClientResolution::deny(
                    OAUTH_ERR_INVALID_CLIENT,
                    "client metadata declares both `jwks` and `jwks_uri` — a client \
                     must publish exactly one key set",
                );
            }
            (None, None) => {
                return ClientResolution::deny(
                    OAUTH_ERR_INVALID_CLIENT,
                    "a `private_key_jwt` client must declare `jwks` or `jwks_uri` — \
                     without a key set it could never authenticate",
                );
            }
            (Some(inline), None) => {
                // The inline set is parsed **now**, so a malformed one refuses
                // at resolution (and is cached negatively) rather than at the
                // client's first assertion, where the failure would read as the
                // assertion's fault.
                match parse_jwks(&inline.to_string()) {
                    Ok(keys) => jwks = keys,
                    Err(why) => return ClientResolution::deny(OAUTH_ERR_INVALID_CLIENT, &why),
                }
            }
            // The caller fetches it and completes the client through
            // `attach_client_jwks`; the empty set here is not a resting state.
            (None, Some(_)) => {}
        }
    }

    // The Fauna extension's form: a compact JWS string or nothing. A bare
    // object is an unsigned manifest and refuses at resolution
    // (`third-party-kinds.md` § The manifest); the signature itself is the
    // resolving server's to verify, against the document's host.
    let fauna_manifest = match doc.fauna {
        None | Some(serde_json::Value::Null) => None,
        Some(serde_json::Value::String(jws)) => Some(jws),
        Some(_) => {
            return ClientResolution::deny(
                OAUTH_ERR_INVALID_CLIENT,
                "client metadata `fauna` must be a compact JWS string — an unsigned \
                 manifest is refused",
            );
        }
    };

    ClientResolution::Resolved {
        client: ResolvedClient {
            client_id,
            client_name: non_empty(doc.client_name),
            client_uri: non_empty(doc.client_uri),
            logo_uri: non_empty(doc.logo_uri),
            tos_uri: non_empty(doc.tos_uri),
            policy_uri: non_empty(doc.policy_uri),
            redirect_uris,
            declared_scopes,
            confidential,
            jwks,
            jwks_uri,
            loopback: false,
            fauna_manifest,
        },
    }
}

// ── Redirect-URI matching ────────────────────────────────────────────────────

/// Does `candidate` match one of this client's declared redirect URIs?
///
/// Exact string equality for every ordinary client — the OAuth rule, and the
/// one that leaves no room for a prefix or substring trick to reach an
/// attacker-controlled path.
///
/// The **loopback** client is the single documented exception: the spec has
/// the port ignored, because a development server binds whatever ephemeral
/// port it was given and cannot publish it in advance. The path still matches
/// exactly, and the host is already pinned to the loopback interface by
/// [`is_loopback_redirect_uri`] — so what the exception actually relaxes is
/// only *which port on the user's own machine*, which is not a boundary.
pub fn redirect_uri_matches(client: &ResolvedClient, candidate: &str) -> bool {
    if !client.loopback {
        return client.redirect_uris.iter().any(|u| u == candidate);
    }
    let normalized = strip_authority_port(candidate);
    client
        .redirect_uris
        .iter()
        .any(|u| strip_authority_port(u) == normalized)
}

/// Rewrite a URL with its authority's port removed, for loopback comparison.
/// A URL whose shape this does not recognize is returned unchanged, so it can
/// only ever fail to match — never match something it should not.
fn strip_authority_port(uri: &str) -> String {
    let Some((scheme, after)) = uri.split_once("://") else {
        return uri.to_string();
    };
    let end = after.find(['/', '?', '#']).unwrap_or(after.len());
    let (authority, rest) = after.split_at(end);
    format!("{scheme}://{}{rest}", strip_port(authority))
}

/// Drop a `:port` suffix from an authority, honouring IPv6 brackets
/// (`[::1]:8080` → `[::1]`, and `[::1]` itself is left alone). The brackets
/// stay on because the caller reassembles a URL from the result.
///
/// Delegating to the canonical splitter also tightened two malformed-authority
/// cases in the fail-closed direction this function's caller promises: an
/// unbracketed `::1:8080` used to collapse to the empty string (so two
/// different malformed redirect URIs could normalize to the same
/// `scheme:///path` and *match*), and a non-numeric `host:junk` used to lose
/// its suffix. Both now pass through whole.
fn strip_port(authority: &str) -> &str {
    fauna_core::web::split_host_port(authority).0
}

// ── Shared helpers ───────────────────────────────────────────────────────────

/// Split a space-delimited OAuth `scope` string. One splitter, because the
/// document's `scope` member and a PAR request's `scope` parameter are the
/// same vocabulary and must never be read two ways.
pub(crate) fn split_scope(scope: &str) -> Vec<String> {
    scope.split_whitespace().map(str::to_string).collect()
}

/// Treat an empty string as an absent member — a document that says
/// `"client_name": ""` has told the consent screen nothing, and rendering an
/// empty name beside the origin is worse than rendering only the origin.
fn non_empty(value: Option<String>) -> Option<String> {
    value.filter(|v| !v.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

    fn resolved(plan: ClientIdPlan) -> ResolvedClient {
        match plan {
            ClientIdPlan::Resolved { client } => client,
            other => panic!("expected a resolved client, got {other:?}"),
        }
    }

    fn denial(plan: ClientIdPlan) -> (String, String) {
        match plan {
            ClientIdPlan::Deny { error, description } => (error, description),
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    fn doc_client(body: &str) -> ResolvedClient {
        match parse_client_metadata("https://app.example.com/client.json".into(), body.into()) {
            ClientResolution::Resolved { client } => client,
            other => panic!("expected a resolved client, got {other:?}"),
        }
    }

    /// `GOOD_DOC` as a confidential client, with the key-set members spliced
    /// in. Built from the same document every other test uses, so a change to
    /// the shared shape reaches the confidential cases too.
    fn confidential_doc(key_members: &str) -> String {
        GOOD_DOC.replace(
            "\"token_endpoint_auth_method\": \"none\"",
            &format!("\"token_endpoint_auth_method\": \"private_key_jwt\", {key_members}"),
        )
    }

    fn doc_denial(body: &str) -> String {
        match parse_client_metadata("https://app.example.com/client.json".into(), body.into()) {
            ClientResolution::Deny { description, .. } => description,
            other => panic!("expected a denial, got {other:?}"),
        }
    }

    const GOOD_DOC: &str = r#"{
        "client_id": "https://app.example.com/client.json",
        "client_name": "Example App",
        "logo_uri": "https://app.example.com/logo.png",
        "redirect_uris": ["https://app.example.com/cb"],
        "grant_types": ["authorization_code", "refresh_token"],
        "response_types": ["code"],
        "scope": "atproto transition:generic",
        "dpop_bound_access_tokens": true,
        "application_type": "web",
        "token_endpoint_auth_method": "none"
    }"#;

    // ── client_id classification ─────────────────────────────────────────────

    /// The ordinary case: an https client_id becomes a fetch plan carrying the
    /// URL back **verbatim**, because the string Go GETs must be the string the
    /// document's own `client_id` is compared against.
    #[test]
    fn an_https_client_id_plans_a_fetch_of_itself_verbatim() {
        let url = "https://app.example.com/oauth-client-metadata.json";
        assert_eq!(
            plan_client_id(url.to_string()),
            ClientIdPlan::Fetch {
                url: url.to_string()
            }
        );
    }

    /// Every client_id shape this module refuses **before** any I/O. Each is a
    /// tightening the SSRF guard would not express, because the guard reasons
    /// about the network target and these are about identity.
    #[test]
    fn the_client_id_tightenings_all_refuse_before_any_fetch() {
        for (client_id, expect) in [
            ("http://app.example.com/c.json", "must be an https URL"),
            ("ftp://app.example.com/c.json", "must be an https URL"),
            ("https://app.example.com/c.json#frag", "fragment"),
            ("https://evil.example@good.example/c.json", "userinfo"),
            ("https://app.example.com:8443/c.json", "port number"),
            ("https:///c.json", "names no host"),
        ] {
            let (error, description) = denial(plan_client_id(client_id.to_string()));
            assert_eq!(error, OAUTH_ERR_INVALID_CLIENT, "for {client_id}");
            assert!(
                description.contains(expect),
                "`{client_id}` denied with `{description}`, expected it to mention `{expect}`"
            );
        }
    }

    /// **Security finding, the boundary fix — the mutation's
    /// target.** A control character anywhere in a client_id is refused in
    /// BOTH arms, before any resolution: the loopback probe here is the exact
    /// string the finding proved end to end (the newline rides *inside* the
    /// `scope` value, so the unknown-parameter refusal never saw it,
    /// `split_whitespace` still found the base scope, and the consent card
    /// painted the attacker's "It is asking to:" heading above the real one).
    /// The https arm was bounded, not broken — `safefetch` parses the raw
    /// string with Go's `net/url`, which rejects control characters before
    /// any dial — but a fence that depends on a downstream parser's manners
    /// is not a boundary, so both arms refuse HERE.
    #[test]
    fn a_control_character_in_the_client_id_is_refused_in_both_arms() {
        for client_id in [
            // The loopback arm — proven probe…
            "http://localhost?scope=atproto\nIt is asking to:",
            // …and a later pass's, the one the reviewer re-runs as the mutation:
            // the loopback arm is the REACHABLE one (it resolves with no
            // fetch to screen anything), so it is pinned by name, twice.
            "http://localhost?scope=atproto\nNo connected apps",
            // The https arm, same class.
            "https://app.example.com/c.json\nIt is asking to:",
            // Other C0 controls are the same byte class, both arms.
            "http://localhost?scope=atproto\rx",
            "https://app.example.com/c\tjson",
        ] {
            let (error, description) = denial(plan_client_id(client_id.to_string()));
            assert_eq!(error, OAUTH_ERR_INVALID_CLIENT, "for {client_id:?}");
            assert!(
                description.contains("control character"),
                "`{client_id:?}` denied with `{description}`"
            );
        }
    }

    /// The do-not-cheat control for the refusal above: an honest loopback
    /// client_id — query and all — still resolves **byte for byte unchanged**.
    /// A "fix" that normalized or re-encoded the identity would pass the
    /// hostile cases while making the string the user is told to trust differ
    /// from the identity actually authorized.
    #[test]
    fn an_honest_loopback_client_id_still_resolves_byte_for_byte_unchanged() {
        let honest = "http://localhost?scope=atproto&redirect_uri=http://127.0.0.1/";
        let client = resolved(plan_client_id(honest.to_string()));
        assert_eq!(client.client_id, honest);
    }

    // ── the loopback development client ──────────────────────────────────────

    /// A bare `http://localhost` resolves with **no fetch**, to the spec's
    /// defaults, under a name this server chose.
    #[test]
    fn the_bare_loopback_client_resolves_to_the_spec_defaults_with_no_fetch() {
        for spelling in ["http://localhost", "http://localhost/"] {
            let client = resolved(plan_client_id(spelling.to_string()));
            assert_eq!(client.client_id, spelling);
            assert_eq!(client.client_name.as_deref(), Some(LOOPBACK_CLIENT_NAME));
            assert_eq!(
                client.redirect_uris,
                vec!["http://127.0.0.1/", "http://[::1]/"]
            );
            assert_eq!(client.declared_scopes, vec![SCOPE_ATPROTO_BASE]);
            assert!(client.loopback);
            assert!(!client.confidential);
        }
    }

    /// The query string configures exactly two things, and both land.
    #[test]
    fn the_loopback_query_configures_redirect_uris_and_scope() {
        let client = resolved(plan_client_id(
            "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb\
             &redirect_uri=http%3A%2F%2F%5B%3A%3A1%5D%2Fcb\
             &scope=atproto+transition%3Ageneric"
                .to_string(),
        ));
        assert_eq!(
            client.redirect_uris,
            vec!["http://127.0.0.1/cb", "http://[::1]/cb"]
        );
        assert_eq!(
            client.declared_scopes,
            vec!["atproto".to_string(), "transition:generic".to_string()]
        );
    }

    /// **The carve-out's security boundary.** `http://localhost` is an identity
    /// anybody may claim, so it may only ever redirect to the user's own
    /// machine. A declared redirect naming anything else is refused — otherwise
    /// an attacker holds a consent-screen identity that hands the authorization
    /// response to a host they control.
    #[test]
    fn a_loopback_client_may_not_declare_a_non_loopback_redirect() {
        for hostile in [
            "https://evil.example/cb",
            "http://evil.example/cb",
            "http://169.254.169.254/cb",
            "http://10.0.0.5/cb",
            "myapp://cb",
            "http://localhost/cb",
        ] {
            let encoded = hostile.replace(':', "%3A").replace('/', "%2F");
            let (error, description) = denial(plan_client_id(format!(
                "http://localhost?redirect_uri={encoded}"
            )));
            assert_eq!(error, OAUTH_ERR_INVALID_CLIENT, "for {hostile}");
            assert!(
                description.contains("127.0.0.1 or [::1]"),
                "`{hostile}` denied with `{description}`"
            );
        }
    }

    /// Spellings that look like the loopback client but are not it fall through
    /// to the https arm and are denied by name — never silently treated as the
    /// development identity.
    #[test]
    fn near_miss_loopback_spellings_are_not_the_loopback_client() {
        for near in [
            "http://localhost:8080",
            "http://localhost/app",
            "http://127.0.0.1",
            "http://localhost.evil.example",
            "https://localhost",
        ] {
            let plan = plan_client_id(near.to_string());
            assert!(
                !matches!(plan, ClientIdPlan::Resolved { .. }),
                "`{near}` must not resolve as the loopback development client, got {plan:?}"
            );
        }
    }

    /// An unrecognized loopback parameter refuses rather than being ignored:
    /// for this client the client_id *is* the document, so a parameter this
    /// server did not read is a configuration the client wrongly believes it
    /// made.
    #[test]
    fn an_unknown_loopback_parameter_refuses() {
        let (_, description) = denial(plan_client_id("http://localhost?logo_uri=x".to_string()));
        assert!(description.contains("logo_uri"), "got `{description}`");
    }

    /// A loopback `scope` that drops the base scope is refused — the same
    /// requirement a fetched document carries.
    #[test]
    fn a_loopback_scope_without_the_base_scope_refuses() {
        let (_, description) = denial(plan_client_id(
            "http://localhost?scope=transition%3Ageneric".to_string(),
        ));
        assert!(description.contains("atproto"), "got `{description}`");
    }

    /// The base-scope rule is the ATProto family's (TP6): a client declaring
    /// only the OIDC family resolves without `atproto`, and one mixing a
    /// non-OIDC scope in still needs it.
    #[test]
    fn an_oidc_only_loopback_client_needs_no_base_scope() {
        let ClientIdPlan::Resolved { client } = plan_client_id(
            "http://localhost?scope=openid%20profile&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb"
                .to_string(),
        ) else {
            panic!("an OIDC-only loopback client must resolve");
        };
        assert_eq!(client.declared_scopes, vec!["openid", "profile"]);
        let (_, description) = denial(plan_client_id(
            "http://localhost?scope=openid%20transition%3Ageneric".to_string(),
        ));
        assert!(description.contains("atproto"), "got `{description}`");
    }

    /// The Fauna family is the nest's, not the PDS's: a principal declaring
    /// only it (and OIDC) needs no `atproto` either.
    #[test]
    fn a_fauna_family_loopback_client_needs_no_base_scope() {
        let ClientIdPlan::Resolved { client } = plan_client_id(
            "http://localhost?scope=fauna%3Afeed%3Aread%20openid&redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb"
                .to_string(),
        ) else {
            panic!("a Fauna-family loopback client must resolve");
        };
        assert_eq!(client.declared_scopes, vec!["fauna:feed:read", "openid"]);
    }

    // ── metadata document validation ─────────────────────────────────────────

    #[test]
    fn a_well_formed_document_resolves_with_its_display_members() {
        let client = doc_client(GOOD_DOC);
        assert_eq!(client.client_id, "https://app.example.com/client.json");
        assert_eq!(client.client_name.as_deref(), Some("Example App"));
        assert_eq!(
            client.logo_uri.as_deref(),
            Some("https://app.example.com/logo.png")
        );
        assert_eq!(client.redirect_uris, vec!["https://app.example.com/cb"]);
        assert_eq!(
            client.declared_scopes,
            vec!["atproto".to_string(), "transition:generic".to_string()]
        );
        assert!(!client.confidential);
        assert!(!client.loopback);
    }

    /// **The self-authenticating property.** A document is only this client's
    /// if its own `client_id` equals the URL it was served at — otherwise any
    /// host could serve a document claiming to be a well-known application.
    #[test]
    fn a_document_claiming_a_different_client_id_refuses() {
        let body = GOOD_DOC.replace(
            "https://app.example.com/client.json",
            "https://bank.example.com/client.json",
        );
        assert!(
            doc_denial(&body).contains("does not match the URL it was fetched from"),
            "got `{}`",
            doc_denial(&body)
        );
    }

    /// Every required member, one at a time. Each removal must refuse — a
    /// table rather than one test per member, so adding a requirement is one
    /// row.
    #[test]
    fn every_required_member_is_actually_required() {
        for (member, expect) in [
            ("client_id", "no `client_id`"),
            ("dpop_bound_access_tokens", "dpop_bound_access_tokens"),
            ("response_types", "`response_types` must include"),
            ("grant_types", "`grant_types` must include"),
            ("redirect_uris", "at least one `redirect_uri`"),
            ("scope", "must include the base `atproto` scope"),
        ] {
            let body = strip_member(GOOD_DOC, member);
            let description = doc_denial(&body);
            assert!(
                description.contains(expect),
                "removing {member} denied with `{description}`, expected `{expect}`"
            );
        }
    }

    /// `dpop_bound_access_tokens: false` is a *present* member with the wrong
    /// value — a different failure from omitting it, and one a real client
    /// could plausibly publish.
    #[test]
    fn a_document_declining_dpop_refuses() {
        let body = GOOD_DOC.replace(
            "\"dpop_bound_access_tokens\": true",
            "\"dpop_bound_access_tokens\": false",
        );
        assert!(doc_denial(&body).contains("dpop_bound_access_tokens"));
    }

    #[test]
    fn a_confidential_client_is_recognized_as_one() {
        let body = confidential_doc(
            r#""jwks": {"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1",
                                 "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                                 "y": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}"#,
        );
        let client = doc_client(&body);
        assert!(client.confidential);
        assert_eq!(client.jwks.len(), 1);
        assert_eq!(client.jwks[0].kid.as_deref(), Some("k1"));
        assert_eq!(client.jwks[0].x.len(), 32);
    }

    /// **A confidential client with no key set is refused at RESOLUTION**, not
    /// at its first assertion.
    ///
    /// Its document says it authenticates with a key; a document that then
    /// names no key describes a client that could never authenticate at all.
    /// Refusing here means the client author sees the configuration error
    /// directly, and — because the refusal is cached negatively — it costs one
    /// fetch rather than one per request. Refusing later would surface as an
    /// assertion failure, which reads as *the assertion's* fault.
    #[test]
    fn a_confidential_client_must_declare_exactly_one_key_set() {
        let none = GOOD_DOC.replace(
            "\"token_endpoint_auth_method\": \"none\"",
            "\"token_endpoint_auth_method\": \"private_key_jwt\"",
        );
        assert!(doc_denial(&none).contains("must declare `jwks` or `jwks_uri`"));

        // Two answers to "which keys sign my assertions" is not a merge
        // problem — picking one would be this server deciding which of the
        // client's own statements to believe.
        let both = confidential_doc(
            r#""jwks": {"keys": [{"kty": "EC", "crv": "P-256", "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", "y": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]},
               "jwks_uri": "https://app.example.com/jwks.json""#,
        );
        assert!(doc_denial(&both).contains("both"));

        // A `jwks_uri` alone resolves with an empty set — the caller fetches it
        // and completes the client through `attach_client_jwks`.
        let uri = confidential_doc(r#""jwks_uri": "https://app.example.com/jwks.json""#);
        let client = doc_client(&uri);
        assert!(client.jwks.is_empty());
        assert_eq!(
            client.jwks_uri.as_deref(),
            Some("https://app.example.com/jwks.json")
        );
    }

    /// A key set carrying keys this server cannot verify with is normal; a set
    /// with **nothing** it can verify with is not.
    #[test]
    fn unusable_keys_are_skipped_but_a_set_with_no_es256_key_refuses() {
        let mixed = confidential_doc(
            r#""jwks": {"keys": [
                 {"kty": "RSA", "kid": "rsa", "n": "abc", "e": "AQAB"},
                 {"kty": "EC", "crv": "P-384", "kid": "wrong-curve", "x": "a", "y": "b"},
                 {"kty": "EC", "crv": "P-256", "kid": "enc-only", "use": "enc",
                  "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                  "y": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"},
                 {"kty": "EC", "crv": "P-256", "kid": "sig",
                  "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                  "y": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}"#,
        );
        let client = doc_client(&mixed);
        assert_eq!(client.jwks.len(), 1, "only the signing P-256 key is usable");
        assert_eq!(client.jwks[0].kid.as_deref(), Some("sig"));

        let empty =
            confidential_doc(r#""jwks": {"keys": [{"kty": "RSA", "n": "abc", "e": "AQAB"}]}"#);
        assert!(doc_denial(&empty).contains("ES256"));
    }

    /// A client that publishes its private key has a key that must be treated
    /// as compromised — verifying against its public half would let it keep
    /// authenticating while we hold a secret we were never meant to see.
    #[test]
    fn a_key_set_carrying_private_material_is_refused_not_ignored() {
        let leaky = confidential_doc(
            r#""jwks": {"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1",
                                 "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                                 "y": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                                 "d": "AgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}"#,
        );
        assert!(doc_denial(&leaky).contains("private key material"));
    }

    /// Two keys under one `kid` make selection depend on document order, and an
    /// order an attacker who can append to the set controls is not a selection
    /// rule.
    #[test]
    fn a_key_set_with_a_duplicate_kid_refuses() {
        let dup = confidential_doc(
            r#""jwks": {"keys": [
                 {"kty": "EC", "crv": "P-256", "kid": "same",
                  "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                  "y": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"},
                 {"kty": "EC", "crv": "P-256", "kid": "same",
                  "x": "AgAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                  "y": "AwAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}"#,
        );
        assert!(doc_denial(&dup).contains("document order"));
    }

    /// A malformed *usable-looking* key refuses rather than being skipped —
    /// skipping it would leave the client authenticating under whichever other
    /// key happened to parse, a silent downgrade of the set it published.
    #[test]
    fn a_short_coordinate_refuses_the_whole_set_rather_than_skipping_the_key() {
        let short = confidential_doc(
            r#""jwks": {"keys": [{"kty": "EC", "crv": "P-256", "kid": "k1",
                                 "x": "AAAA", "y": "AQAA"}]}"#,
        );
        assert!(doc_denial(&short).contains("zero-padded"));
    }

    /// The completion path for a `jwks_uri` client, and its two refusals.
    #[test]
    fn attaching_a_fetched_key_set_completes_a_confidential_client() {
        let client = doc_client(&confidential_doc(
            r#""jwks_uri": "https://app.example.com/jwks.json""#,
        ));
        let body = r#"{"keys": [{"kty": "EC", "crv": "P-256", "kid": "fetched",
                                 "x": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
                                 "y": "AQAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"}]}"#;
        let ClientResolution::Resolved { client: done } =
            attach_client_jwks(client.clone(), body.into())
        else {
            panic!("a well-formed fetched key set must complete the client");
        };
        assert_eq!(done.jwks.len(), 1);
        assert_eq!(done.jwks[0].kid.as_deref(), Some("fetched"));
        // Everything else survives — the completion adds keys, it does not
        // rebuild the client.
        assert_eq!(done.client_id, client.client_id);
        assert_eq!(done.declared_scopes, client.declared_scopes);

        let ClientResolution::Deny { description, .. } =
            attach_client_jwks(client, "not json".into())
        else {
            panic!("a malformed fetched key set must refuse");
        };
        assert!(description.contains("not valid JSON"), "{description}");

        // `confidential` is the single owner of whether a key set means
        // anything — a public client never acquires one.
        let ClientResolution::Deny { description, .. } =
            attach_client_jwks(doc_client(GOOD_DOC), "{\"keys\":[]}".into())
        else {
            panic!("a public client must not acquire a key set");
        };
        assert!(
            description.contains("does not authenticate"),
            "{description}"
        );
    }

    #[test]
    fn an_unrecognized_auth_method_or_application_type_refuses() {
        let secret = GOOD_DOC.replace(
            "\"token_endpoint_auth_method\": \"none\"",
            "\"token_endpoint_auth_method\": \"client_secret_basic\"",
        );
        assert!(doc_denial(&secret).contains("client_secret_basic"));

        let app_type = GOOD_DOC.replace(
            "\"application_type\": \"web\"",
            "\"application_type\": \"tv\"",
        );
        assert!(doc_denial(&app_type).contains("`web` or `native`"));
    }

    /// A member the format grows must not break a client — unknown members are
    /// ignored, which is the opposite posture from the loopback query's
    /// (documented at both sites).
    #[test]
    fn an_unknown_document_member_is_ignored_not_refused() {
        let body = GOOD_DOC.replace(
            "\"application_type\": \"web\"",
            "\"application_type\": \"web\", \"future_member\": {\"a\": 1}",
        );
        assert_eq!(
            doc_client(&body).client_name.as_deref(),
            Some("Example App")
        );
    }

    /// The `fauna` member is the signed kind manifest: a compact JWS string is
    /// carried verbatim for the resolving server to verify; a bare object is
    /// an unsigned manifest and refuses (`third-party-kinds.md` § The
    /// manifest).
    #[test]
    fn the_fauna_member_is_a_jws_string_or_refuses() {
        let with = |member: &str| {
            GOOD_DOC.replace(
                "\"application_type\": \"web\"",
                &format!("\"application_type\": \"web\", \"fauna\": {member}"),
            )
        };
        assert_eq!(doc_client(GOOD_DOC).fauna_manifest, None);
        assert_eq!(
            doc_client(&with("\"aGVhZA.cGF5bG9hZA.c2ln\""))
                .fauna_manifest
                .as_deref(),
            Some("aGVhZA.cGF5bG9hZA.c2ln")
        );
        assert!(doc_denial(&with("{\"version\": 1}")).contains("unsigned manifest"));
        assert!(doc_denial(&with("[]")).contains("unsigned manifest"));
    }

    #[test]
    fn a_body_that_is_not_json_refuses_rather_than_panicking() {
        assert!(doc_denial("<html>404</html>").contains("not valid JSON"));
    }

    /// An empty display string is an absent one — rendering `""` beside the
    /// origin is worse than rendering the origin alone.
    #[test]
    fn empty_display_members_read_as_absent() {
        let body = GOOD_DOC.replace("\"Example App\"", "\"   \"");
        assert_eq!(doc_client(&body).client_name, None);
    }

    // ── redirect matching ────────────────────────────────────────────────────

    #[test]
    fn an_ordinary_client_matches_its_redirect_uris_exactly() {
        let client = doc_client(GOOD_DOC);
        assert!(redirect_uri_matches(&client, "https://app.example.com/cb"));
        for near in [
            "https://app.example.com/cb2",
            "https://app.example.com/cb/",
            "https://app.example.com/cb?x=1",
            "https://app.example.com:443/cb",
            "https://evil.example/cb",
        ] {
            assert!(
                !redirect_uri_matches(&client, near),
                "`{near}` must not match an exactly-declared redirect URI"
            );
        }
    }

    /// The loopback exception, both halves: the port is ignored, the path is
    /// **not**.
    #[test]
    fn the_loopback_client_ignores_the_port_but_not_the_path() {
        let client = resolved(plan_client_id(
            "http://localhost?redirect_uri=http%3A%2F%2F127.0.0.1%2Fcb".to_string(),
        ));
        assert!(redirect_uri_matches(&client, "http://127.0.0.1:5173/cb"));
        assert!(redirect_uri_matches(&client, "http://127.0.0.1/cb"));
        assert!(!redirect_uri_matches(
            &client,
            "http://127.0.0.1:5173/other"
        ));
        assert!(!redirect_uri_matches(&client, "http://[::1]:5173/cb"));
    }

    /// Port-stripping must honour IPv6 brackets — `[::1]` is not `[` with a
    /// port of `:1]`.
    #[test]
    fn port_stripping_honours_ipv6_brackets() {
        let client = resolved(plan_client_id(
            "http://localhost?redirect_uri=http%3A%2F%2F%5B%3A%3A1%5D%2Fcb".to_string(),
        ));
        assert!(redirect_uri_matches(&client, "http://[::1]:9999/cb"));
        assert!(redirect_uri_matches(&client, "http://[::1]/cb"));
        assert_eq!(strip_port("[::1]"), "[::1]");
        assert_eq!(strip_port("[::1]:8080"), "[::1]");
        assert_eq!(strip_port("127.0.0.1:80"), "127.0.0.1");
        assert_eq!(strip_port("app.example.com"), "app.example.com");
    }

    /// Remove a top-level member from a JSON fixture by name, for the
    /// required-member table. Goes through `serde_json` rather than string
    /// surgery: an earlier hand-rolled cut sliced at the first comma after the
    /// key, which lands *inside* an array value and produced a JSON-parse
    /// failure that the table then mistook for the absence refusal.
    fn strip_member(body: &str, key: &str) -> String {
        let mut doc: Value = serde_json::from_str(body).expect("fixture is valid JSON");
        let object = doc.as_object_mut().expect("fixture is a JSON object");
        assert!(
            object.remove(key).is_some(),
            "fixture has no `{key}` member to remove"
        );
        doc.to_string()
    }
}
