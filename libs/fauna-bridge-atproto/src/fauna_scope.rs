//! The Fauna scope family — `fauna:<plane>:<verb>[:<qualifier>]`.
//!
//! `docs/goal/behavior/authorization-server.md` § Scope grammar → *The Fauna
//! family, exactly* owns every rule this module follows. The family is a
//! **closed table**: a scope string is grantable only when it matches an arm
//! below, and an arm exists only while a kind in the nest's `ThirdParty`
//! ceiling names it — or, for a bridge-doored arm, while the BridgeMda relay
//! kind that admits its token exists ([`FaunaScopeArm::door`]; a nest-side
//! sweep test holds both sets). A plane the goal doc lists and no slice has
//! built yet — `post`, `follow` … — matches no arm and is `invalid_scope` at PAR; so does a
//! built arm's qualifier the arm does not accept (an `identity:op` class not
//! yet built, or a sovereign operation's name, which never will be).
//!
//! **A shipped arm's reach never widens.** A grant is frozen at its ceremony,
//! so mapping a new kind onto an existing string would enlarge every
//! outstanding grant with no consent event. New reach is a new verb or a new
//! qualifier, with a card row of its own.
//!
//! The two doors every caller already uses stay the only ones:
//! [`crate::authz::scope_grants_something`] answers by arm membership and
//! [`crate::authz::describe_scope`] renders the arm's card row. This module is
//! pure and wasm-clean, like the rest of the always-on core.

/// The prefix every Fauna-family scope carries — the family's membership.
///
/// Membership is not grantability: `fauna:events:subscribe` is a
/// Fauna-family scope (an access token for it names the nest's issuer) and
/// also one no arm grants today. The token's reader is the arm's
/// [`FaunaScopeArm::door`] — the nest's dispatch for most arms, the MDA's
/// WebDAV server for `fauna:folder:read:<id>`.
pub const FAUNA_SCOPE_PREFIX: &str = "fauna:";

/// `fauna:feed:read` — the nest's public timelines.
pub const SCOPE_FEED_READ: &str = "fauna:feed:read";

/// `fauna:identity:op:nostr.sign_event` — ask the nest to sign Nostr events
/// with the user's deposited key (`key-material-hierarchy.md` § *The oracle*).
pub const SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT: &str = "fauna:identity:op:nostr.sign_event";

/// `fauna:identity:op:nostr.nip44` — ask the nest to NIP-44 encrypt and
/// decrypt with the user's deposited key.
pub const SCOPE_IDENTITY_OP_NOSTR_NIP44: &str = "fauna:identity:op:nostr.nip44";

/// `fauna:identity:op:atproto.service_auth` — ask the atproto bridge's
/// custodian for a service-auth token, bounded by the manifest's declared
/// `service_auth` set (`third-party.md` § The manifest). The class is not in
/// the operation-class table yet, so no arm grants it; the resolver already
/// refuses a document asking for it without declaring the set.
pub const SCOPE_IDENTITY_OP_ATPROTO_SERVICE_AUTH: &str = "fauna:identity:op:atproto.service_auth";

/// `fauna:records:rw` — the `records` arm's plane and verb, as discovery
/// advertises it. Never grantable as written: the arm requires its qualifier
/// (`fauna:records:rw:<ext kind | ext.<publisher>.*>`), which is the client's.
pub const SCOPE_RECORDS_RW: &str = "fauna:records:rw";

/// `fauna:conversations:bridge` — carry the account's bridged conversations
/// (`apps/bridges.md` § Bridge-kind catalogue → Phase G).
pub const SCOPE_CONVERSATIONS_BRIDGE: &str = "fauna:conversations:bridge";

/// `fauna:folder:deposit` — the `folder_deposit` arm's plane and verb, as
/// discovery advertises it and a client document declares it. Requestable
/// bare, never grantable as written: the arm requires its qualifier, the
/// folder's row id (`fauna:folder:deposit:<id>`), which the user chooses at
/// consent ([`qualify_bare`]).
pub const SCOPE_FOLDER_DEPOSIT: &str = "fauna:folder:deposit";

/// `fauna:folder:read` — the `folder_read` arm's plane and verb, as discovery
/// advertises it. Never grantable as written: the arm requires its qualifier,
/// the folder's row id (`fauna:folder:read:<id>`).
pub const SCOPE_FOLDER_READ: &str = "fauna:folder:read";

/// `fauna:events:subscribe` — be told which of the principal's own scopes
/// changed (`transport.md` § Push events → *Third-party event doors*).
pub const SCOPE_EVENTS_SUBSCRIBE: &str = "fauna:events:subscribe";

/// The BridgeMda relay kind that admits a `fauna:folder:read` token at the
/// MDA's WebDAV door (`webdav-server.md` § Key model → *A principal's read*):
/// the read arm's [`Door::Mda`]. Spelled here, beside the arm, because the
/// grammar crate sits below the protocol crate; the nest's allowlist and the
/// Go MDA's method table carry the same string, and the nest's sweep test
/// holds them equal.
pub const WEBDAV_ADMIT_PRINCIPAL_KIND: &str = "fauna.bridges.webdav_admit_principal";

/// Where an arm's token is honoured (`authorization-server.md` § Scope
/// grammar → *A closed table*, amended 2026-10-05): every arm has exactly one
/// door.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Door {
    /// The nest's own dispatch: the arm exists while a kind in the
    /// `ThirdParty` ceiling names it.
    Nest,
    /// A bridge's resource server: the arm exists while the BridgeMda relay
    /// kind that admits its token exists — `relay_kind` names it.
    Mda { relay_kind: &'static str },
}

/// One built arm of the family. Closed: a new arm is a new variant, added in
/// the same change as the nest-side ceiling kind that names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FaunaScopeArm {
    /// `fauna:feed:read` — no qualifier, no grant twin. The nest's public
    /// timelines as any reader of them would get them: public posts only,
    /// nothing of the account's own state.
    FeedRead,
    /// `fauna:identity:op:<class>` — the keyless oracle arm
    /// (`key-material-hierarchy.md` § Audience: deployment infrastructure →
    /// *The oracle*). The qualifier names one built
    /// [`fauna_core::identity_op::IdentityOpClass`]; a sovereign or unknown
    /// name matches no arm. The principal asks the key's custodian to perform
    /// the class and never holds the key.
    IdentityOp,
    /// `fauna:records:rw:<qualifier>` — read and write the principal's own
    /// third-party kinds (`third-party-kinds.md` § The record doors). The
    /// qualifier is REQUIRED and is one `ext.*` kind or `ext.<publisher>.*`,
    /// parsed by [`fauna_core::ext_kind::ExtQualifier`] — the wildcard covers
    /// one publisher's kinds structurally, never by string prefix. Whether a
    /// wildcard names the requesting document's own host is PAR's check
    /// ([`records_qualifier`]), not the grammar's.
    Records,
    /// `fauna:conversations:bridge` — no qualifier, no grant twin. The
    /// bridge's six Phase G kinds plus the recipient-key read its deposits are
    /// sealed with (`apps/bridges.md` § Bridge-kind catalogue → Phase G). The
    /// nest is blind both ways: inbound is sealed by the bridge to the user's
    /// recipient key, outbound by the user's app to the bridge's own key — so
    /// no key of the account's is wrapped to the principal.
    ConversationsBridge,
    /// `fauna:folder:deposit:<folder id>` — post files into one of the
    /// account's folders, write-only and blind (`file-sync.md` § Third-party
    /// deposit ingress). The qualifier is REQUIRED and is the folder's row id
    /// in canonical decimal ([`folder_deposit_qualifier`]). The nest seals
    /// what is deposited to the owner's recipient key; the principal reads
    /// nothing and holds no key.
    FolderDeposit,
    /// `fauna:folder:read:<folder id>` — read the files in one of the
    /// account's folders over the MDA's WebDAV door (`webdav-server.md`
    /// § Key model → *A principal's read*). The qualifier is REQUIRED, the
    /// deposit arm's canonical decimal row id ([`folder_read_qualifier`]).
    /// Its twin is keyed — the `content.read{folder, set}` grant, one wrap
    /// per content-key generation — and its door is the MDA, not the nest
    /// ([`Door::Mda`]). Form-gated at PAR: a remote-form (confidential)
    /// client is refused it ([`FaunaScopeArm::refused_to_remote_form`]).
    FolderRead,
    /// `fauna:events:subscribe` — no qualifier, no grant twin. The events
    /// doors (`transport.md` § Push events → *Third-party event doors*): the
    /// scope-tagged change nudge for exactly the scopes the session's other
    /// arms let it list ([`event_reaches`]), never content. It covers the
    /// `fauna.events.poll` kind; the filtered push on a principal session and
    /// the HTTP long-poll are that kind's other two faces.
    EventsSubscribe,
}

impl FaunaScopeArm {
    /// Every built arm, in the order discovery advertises them.
    pub const ALL: &'static [FaunaScopeArm] = &[
        FaunaScopeArm::FeedRead,
        FaunaScopeArm::IdentityOp,
        FaunaScopeArm::Records,
        FaunaScopeArm::ConversationsBridge,
        FaunaScopeArm::FolderDeposit,
        FaunaScopeArm::FolderRead,
        FaunaScopeArm::EventsSubscribe,
    ];

    /// The arm's plane and verb.
    #[must_use]
    pub const fn plane_verb(self) -> (&'static str, &'static str) {
        match self {
            FaunaScopeArm::FeedRead => ("feed", "read"),
            FaunaScopeArm::IdentityOp => ("identity", "op"),
            FaunaScopeArm::Records => ("records", "rw"),
            FaunaScopeArm::ConversationsBridge => ("conversations", "bridge"),
            FaunaScopeArm::FolderDeposit => ("folder", "deposit"),
            FaunaScopeArm::FolderRead => ("folder", "read"),
            FaunaScopeArm::EventsSubscribe => ("events", "subscribe"),
        }
    }

    /// Where the arm's token is honoured — exactly one door per arm. Every
    /// arm but `FolderRead` is the nest's; `FolderRead` is the MDA's WebDAV
    /// server, admitted through [`WEBDAV_ADMIT_PRINCIPAL_KIND`].
    #[must_use]
    pub const fn door(self) -> Door {
        match self {
            FaunaScopeArm::FolderRead => Door::Mda {
                relay_kind: WEBDAV_ADMIT_PRINCIPAL_KIND,
            },
            FaunaScopeArm::FeedRead
            | FaunaScopeArm::IdentityOp
            | FaunaScopeArm::Records
            | FaunaScopeArm::ConversationsBridge
            | FaunaScopeArm::FolderDeposit
            | FaunaScopeArm::EventsSubscribe => Door::Nest,
        }
    }

    /// Whether PAR refuses the arm to a remote-form principal — a
    /// confidential client, `remote` by rule 3's derivation
    /// ([`crate::oauth_client::ResolvedClient::is_remote_form`]). TP4 grants a
    /// remote principal a read only over a folder it created, and no door
    /// lets a principal create one (`authorization-server.md` § Scope grammar
    /// → *The read arm*).
    #[must_use]
    pub const fn refused_to_remote_form(self) -> bool {
        matches!(self, FaunaScopeArm::FolderRead)
    }

    /// Whether the arm takes a qualifier. A string carrying one where the arm
    /// takes none — or none where it requires one — matches no arm.
    #[must_use]
    pub const fn takes_qualifier(self) -> bool {
        match self {
            FaunaScopeArm::FeedRead
            | FaunaScopeArm::ConversationsBridge
            | FaunaScopeArm::EventsSubscribe => false,
            FaunaScopeArm::IdentityOp
            | FaunaScopeArm::Records
            | FaunaScopeArm::FolderDeposit
            | FaunaScopeArm::FolderRead => true,
        }
    }

    /// Whether the arm accepts `qualifier` — the arm's own value check on top
    /// of the family's charset. `FeedRead` takes none; `IdentityOp` takes
    /// exactly the name of a built operation class, so a sovereign
    /// operation's name is refused here, at the grammar, before any consent
    /// card renders.
    #[must_use]
    pub fn accepts_qualifier(self, qualifier: Option<&str>) -> bool {
        match (self, qualifier) {
            (
                FaunaScopeArm::FeedRead
                | FaunaScopeArm::ConversationsBridge
                | FaunaScopeArm::EventsSubscribe,
                q,
            ) => q.is_none(),
            (FaunaScopeArm::IdentityOp, None) => false,
            (FaunaScopeArm::IdentityOp, Some(q)) => {
                fauna_core::identity_op::IdentityOpClass::parse(q).is_ok()
            }
            (FaunaScopeArm::Records, None) => false,
            (FaunaScopeArm::Records, Some(q)) => {
                q.parse::<fauna_core::ext_kind::ExtQualifier>().is_ok()
            }
            (FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead, None) => false,
            (FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead, Some(q)) => {
                parse_folder_id(q).is_some()
            }
        }
    }

    /// Whether the arm's qualifier is the **user's** — a row of the account's,
    /// different per user and per nest, which no static client document can
    /// declare (`authorization-server.md` § Scope grammar → *The folder
    /// plane's qualifier is the user's*). The folder plane's two arms; every
    /// other qualifier is the client's (`records`) or the server's
    /// (`identity:op`, `feed:read:account`).
    /// Such an arm is declared and may be requested **bare**
    /// ([`user_qualified_bare_arm`]); the user's choice at consent qualifies
    /// it ([`qualify_bare`]).
    #[must_use]
    pub const fn qualifier_is_users(self) -> bool {
        matches!(
            self,
            FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead
        )
    }

    /// Whether a grant under this arm also wraps keys to the principal (the
    /// arm's *grant twin*, `encryption-at-rest.md` § Capability tiering →
    /// *Third-party holders*). The card row says so either way.
    ///
    /// `IdentityOp`'s twin is **keyless**: the user's device mints an
    /// `identity.op` grant naming the class, which wraps nothing — it is the
    /// client-authoritative audit and revocation record the custodian
    /// re-resolves at every operation. `Records`' twin is per covered kind a
    /// `content.read` tuple wrapping the kind's delegable pair and a keyless
    /// `content.write` tuple naming the principal's writer key.
    /// `FolderDeposit`'s twin is keyless too: one `deposit` tuple naming the
    /// folder, the audit + revocation record the deposit door re-resolves.
    /// `FolderRead`'s twin is keyed: the `content.read{folder, set}` tuple
    /// with one wrap per content-key generation.
    #[must_use]
    pub const fn has_grant_twin(self) -> bool {
        match self {
            FaunaScopeArm::FeedRead
            | FaunaScopeArm::ConversationsBridge
            | FaunaScopeArm::EventsSubscribe => false,
            FaunaScopeArm::IdentityOp
            | FaunaScopeArm::Records
            | FaunaScopeArm::FolderDeposit
            | FaunaScopeArm::FolderRead => true,
        }
    }

    /// The scope strings discovery advertises for the arm: the arm itself for
    /// one that takes no qualifier, one string per accepted qualifier for an
    /// arm whose qualifiers are a closed set, and the bare plane and verb for
    /// `Records`, whose qualifier is the client's own kind.
    #[must_use]
    pub const fn advertised(self) -> &'static [&'static str] {
        match self {
            FaunaScopeArm::FeedRead => &[SCOPE_FEED_READ],
            FaunaScopeArm::IdentityOp => &[
                SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT,
                SCOPE_IDENTITY_OP_NOSTR_NIP44,
            ],
            FaunaScopeArm::Records => &[SCOPE_RECORDS_RW],
            FaunaScopeArm::ConversationsBridge => &[SCOPE_CONVERSATIONS_BRIDGE],
            FaunaScopeArm::FolderDeposit => &[SCOPE_FOLDER_DEPOSIT],
            FaunaScopeArm::FolderRead => &[SCOPE_FOLDER_READ],
            FaunaScopeArm::EventsSubscribe => &[SCOPE_EVENTS_SUBSCRIBE],
        }
    }

    /// Grantable strings exemplifying the arm — what a test sweeps when it
    /// needs every arm to match, render and grant. The advertised strings
    /// wherever they are grantable as written; one qualified string for
    /// `Records`, whose advertised form is not.
    #[must_use]
    pub const fn examples(self) -> &'static [&'static str] {
        match self {
            FaunaScopeArm::Records => &[
                "fauna:records:rw:ext.app.example.*",
                "fauna:records:rw:ext.app.example.notes",
            ],
            FaunaScopeArm::FolderDeposit => &["fauna:folder:deposit:42"],
            FaunaScopeArm::FolderRead => &["fauna:folder:read:42"],
            other => other.advertised(),
        }
    }
}

/// The publisher a `records` scope's qualifier names — `None` for any other
/// string. PAR holds a wildcard's publisher to the requesting document's host
/// (`authorization-server.md` § Scope grammar → *Wildcards*); a single kind of
/// another publisher is legal at PAR and is the consent's to admit.
#[must_use]
pub fn records_qualifier(scope: &str) -> Option<fauna_core::ext_kind::ExtQualifier> {
    if arm_of(scope)? != FaunaScopeArm::Records {
        return None;
    }
    parse(scope)?.qualifier?.parse().ok()
}

/// Does a change nudge for `scope_tag` reach a session holding `scopes`? The
/// one filter both events doors apply (`transport.md` § Push events →
/// *Third-party event doors*): the session holds `fauna:events:subscribe`
/// **and** the scope is one its other arms let it list — today an
/// `ext:<kind>` scope whose kind a `records` qualifier covers, structurally
/// (the record door's own reach check). Every other tag — `state`, a folder
/// nudge, another publisher's kind — reaches nothing.
#[must_use]
pub fn event_reaches(scopes: &[String], scope_tag: &str) -> bool {
    if !scopes
        .iter()
        .any(|s| arm_of(s) == Some(FaunaScopeArm::EventsSubscribe))
    {
        return false;
    }
    let Some(kind) = scope_tag.strip_prefix(EXT_SCOPE_PREFIX).and_then(|k| {
        // Strict, never repairing: only the canonical spelling is a scope.
        let kind = k.parse::<fauna_core::ext_kind::ExtKind>().ok()?;
        (kind.to_string() == k).then_some(kind)
    }) else {
        return false;
    };
    scopes
        .iter()
        .any(|s| records_qualifier(s).is_some_and(|q| q.covers(&kind)))
}

/// The `ext` scope family's tag (`third-party-kinds.md` § The `ext`
/// sub-scope): `ext:<kind>`, the kind verbatim after it.
const EXT_SCOPE_PREFIX: &str = "ext:";

/// A folder row id in canonical decimal — positive, no sign, no leading
/// zero — so one folder has exactly one scope string.
fn parse_folder_id(q: &str) -> Option<i64> {
    let id = q.parse::<i64>().ok().filter(|id| *id > 0)?;
    (id.to_string() == q).then_some(id)
}

/// The folder a `folder_deposit` scope names — `None` for any other string.
/// What the deposit door matches the request's folder against: the arm-wide
/// gate admits the kind, this is the per-folder half.
#[must_use]
pub fn folder_deposit_qualifier(scope: &str) -> Option<i64> {
    if arm_of(scope)? != FaunaScopeArm::FolderDeposit {
        return None;
    }
    parse_folder_id(parse(scope)?.qualifier?)
}

/// The folder a `folder_read` scope names — `None` for any other string.
/// What the MDA's admission matches a request's folder against.
#[must_use]
pub fn folder_read_qualifier(scope: &str) -> Option<i64> {
    if arm_of(scope)? != FaunaScopeArm::FolderRead {
        return None;
    }
    parse_folder_id(parse(scope)?.qualifier?)
}

/// Whether PAR must refuse `scope` to a remote-form (confidential) client:
/// a string of an arm [`FaunaScopeArm::refused_to_remote_form`] names —
/// qualified, or the bare plane and verb a card-qualified request carries
/// (`authorization-server.md` § Scope grammar → *The folder plane's
/// qualifier is the user's*: the form gate fires on a bare request as on the
/// qualified string).
#[must_use]
pub fn refused_to_remote_form(scope: &str) -> bool {
    FaunaScopeArm::ALL.iter().any(|arm| {
        arm.refused_to_remote_form()
            && (arm_of(scope) == Some(*arm) || arm.advertised().contains(&scope))
    })
}

/// The user-qualified arm `scope` names **bare** — its plane and verb with no
/// qualifier, the string the client document declares and discovery
/// advertises. `None` for any other string, a qualified one included. No arm
/// grants the bare string as written ([`arm_of`] answers `None`): it is a
/// *card-qualified request*, admitted at PAR and qualified by the user's
/// choice at the ceremony (`authorization-server.md` § Scope grammar → *The
/// folder plane's qualifier is the user's*).
#[must_use]
pub fn user_qualified_bare_arm(scope: &str) -> Option<FaunaScopeArm> {
    let parsed = parse(scope)?;
    if parsed.qualifier.is_some() {
        return None;
    }
    FaunaScopeArm::ALL
        .iter()
        .copied()
        .find(|arm| arm.qualifier_is_users() && arm.plane_verb() == (parsed.plane, parsed.verb))
}

/// The bare form of a user-qualified arm's string — `scope` itself when it is
/// bare, its plane and verb when it is qualified. What PAR holds such a
/// request to: the document declares the bare string and the request narrows
/// it. `None` for every other string, so every other arm keeps exact
/// membership.
#[must_use]
pub fn bare_form(scope: &str) -> Option<&str> {
    if user_qualified_bare_arm(scope).is_some() {
        return Some(scope);
    }
    if !arm_of(scope)?.qualifier_is_users() {
        return None;
    }
    let parsed = parse(scope)?;
    let len = FAUNA_SCOPE_PREFIX.len() + parsed.plane.len() + 1 + parsed.verb.len();
    scope.get(..len)
}

/// The arm's string for a bare user-qualified `scope` qualified by the folder
/// the user chose — what the nest records at the ceremony in place of the
/// bare string. `None` when `scope` is not bare user-qualified or `folder_id`
/// is not a qualifier the arm accepts.
#[must_use]
pub fn qualify_bare(scope: &str, folder_id: i64) -> Option<String> {
    let arm = user_qualified_bare_arm(scope)?;
    let qualified = format!("{scope}:{folder_id}");
    (arm_of(&qualified) == Some(arm)).then_some(qualified)
}

/// A consent row's scope set with every bare user-qualified scope qualified by
/// the one folder the card chose — what the nest records at the ceremony
/// (`authorization-server.md` § Scope grammar → *The folder plane's
/// qualifier is the user's*: one choice qualifies every bare folder verb on
/// the card). Every other scope passes untouched, in order. `None` when the
/// set carries no bare user-qualified scope (a choice is refused there) or
/// `folder_id` is not a qualifier its arm accepts.
#[must_use]
pub fn qualify_bare_scopes<S: AsRef<str>>(scopes: &[S], folder_id: i64) -> Option<Vec<String>> {
    let mut qualified_any = false;
    let mut out = Vec::with_capacity(scopes.len());
    for scope in scopes {
        let scope = scope.as_ref();
        if user_qualified_bare_arm(scope).is_some() {
            out.push(qualify_bare(scope, folder_id)?);
            qualified_any = true;
        } else {
            out.push(scope.to_string());
        }
    }
    qualified_any.then_some(out)
}

/// The consent card's row for `scope`: what the token permits, and — in
/// words — what keys are or are not shared. `None` for a string matching no
/// arm; an arm taking a qualifier renders the row its qualifier names, and a
/// bare user-qualified string renders its arm's row (the card's picker names
/// the folder).
#[must_use]
pub fn card_row(scope: &str) -> Option<&'static str> {
    match arm_of(scope).or_else(|| user_qualified_bare_arm(scope))? {
        FaunaScopeArm::FeedRead => {
            Some("Read this server's public timelines (public posts only; no keys are shared)")
        }
        FaunaScopeArm::IdentityOp => {
            fauna_core::identity_op::IdentityOpClass::parse(parse(scope)?.qualifier?)
                .ok()
                .map(fauna_core::identity_op::IdentityOpClass::card_row)
        }
        FaunaScopeArm::Records => Some(
            "Read and write this app's own records in your account (keys to exactly those \
             records are shared with it)",
        ),
        FaunaScopeArm::ConversationsBridge => Some(
            "Carry your conversations on another network to and from this account (this app \
             reads the messages it carries; no keys of yours are shared with it)",
        ),
        FaunaScopeArm::FolderDeposit => Some(
            "Put files into one of your folders, sealed to you on arrival (it cannot see the \
             folder or anything in it; no keys are shared)",
        ),
        FaunaScopeArm::FolderRead => Some(
            "Read the files in one of your folders, everything already in it included (keys to \
             exactly that folder are shared with it)",
        ),
        FaunaScopeArm::EventsSubscribe => Some(
            "Be told when this app's own records in your account change (it learns only that \
             something changed, never what; no keys are shared)",
        ),
    }
}

/// The consent card's row for a `folder_read` scope, naming the folder when
/// the approving app can resolve its id (`authorization-server.md` § Scope
/// grammar → *The read arm*). `folder_name` is the owner's own name for the
/// folder — `None` when the app cannot resolve it, and the row then reads as
/// [`card_row`]'s. `None` for any scope that is not a `folder_read` scope.
#[must_use]
pub fn folder_read_card_row(scope: &str, folder_name: Option<&str>) -> Option<String> {
    folder_read_qualifier(scope)?;
    Some(match folder_name {
        Some(name) => format!(
            "Read the files in your folder \"{name}\", everything already in it included (keys \
             to exactly that folder are shared with it)"
        ),
        None => card_row(scope)?.to_string(),
    })
}

/// The consent card's row for a `records` scope, worded from what the
/// approving device knows about the request (`authorization-server.md`
/// § Scope grammar → *The third arm*: the publisher, how many kinds, that keys
/// to exactly those records are shared — and, for an app that attested no
/// writer key, that it can only read). `kinds` is how many kinds the scope
/// covers once expanded against the app's verified manifest (`None` when it is
/// not known: a wildcard whose manifest did not verify). `None` for any scope
/// that is not a `records` scope — [`card_row`] words it.
#[must_use]
pub fn records_card_row(scope: &str, kinds: Option<usize>, writable: bool) -> Option<String> {
    use fauna_core::ext_kind::ExtQualifier;
    let what = match records_qualifier(scope)? {
        ExtQualifier::Kind(kind) => format!(
            "records of the kind \"{}\" published by {}",
            kind.name(),
            kind.publisher()
        ),
        ExtQualifier::Publisher(publisher) => match kinds {
            Some(1) => format!("1 kind of records published by {publisher}"),
            Some(n) => format!("{n} kinds of records published by {publisher}"),
            None => format!("the records published by {publisher}"),
        },
    };
    Some(if writable {
        format!(
            "Read and write {what} in your account (keys to exactly those records are shared \
             with it)"
        )
    } else {
        format!(
            "Read {what} in your account — read only: it cannot write them (keys to exactly \
             those records are shared with it)"
        )
    })
}

/// A scope string split into the family's three positions.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ParsedFaunaScope<'a> {
    pub plane: &'a str,
    pub verb: &'a str,
    pub qualifier: Option<&'a str>,
}

/// Is `s` a non-empty run of lowercase ASCII letters — a plane or a verb?
fn is_word(s: &str) -> bool {
    !s.is_empty() && s.bytes().all(|b| b.is_ascii_lowercase())
}

/// Is `s` a well-formed qualifier for `plane`: `a–z 0–9 . _ -`, plus — for
/// `records` alone — one trailing `.*`.
fn is_qualifier(plane: &str, s: &str) -> bool {
    let body = match s.strip_suffix(".*") {
        Some(body) if plane == "records" => body,
        _ => s,
    };
    !body.is_empty()
        && body.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-')
        })
}

/// Parse a scope string under the family's grammar. `None` for a string
/// outside the family or one that breaks its charset — never a guess.
#[must_use]
pub fn parse(scope: &str) -> Option<ParsedFaunaScope<'_>> {
    let rest = scope.strip_prefix(FAUNA_SCOPE_PREFIX)?;
    let mut parts = rest.splitn(3, ':');
    let plane = parts.next()?;
    let verb = parts.next()?;
    let qualifier = parts.next();
    if !is_word(plane) || !is_word(verb) {
        return None;
    }
    if let Some(q) = qualifier
        && !is_qualifier(plane, q)
    {
        return None;
    }
    Some(ParsedFaunaScope {
        plane,
        verb,
        qualifier,
    })
}

/// The built arm `scope` matches, if any — the family's whole grantability
/// question. Whole-string: a qualifier where the arm takes none, none where
/// it requires one, or one the arm does not accept matches nothing.
#[must_use]
pub fn arm_of(scope: &str) -> Option<FaunaScopeArm> {
    let parsed = parse(scope)?;
    FaunaScopeArm::ALL.iter().copied().find(|arm| {
        arm.plane_verb() == (parsed.plane, parsed.verb) && arm.accepts_qualifier(parsed.qualifier)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_parse_table() {
        let p = parse("fauna:feed:read").expect("parses");
        assert_eq!((p.plane, p.verb, p.qualifier), ("feed", "read", None));
        let p = parse("fauna:folder:read:abc-123_x.y").expect("parses");
        assert_eq!(p.qualifier, Some("abc-123_x.y"));
        let p = parse("fauna:records:rw:ext.dating.example.*").expect("records takes .*");
        assert_eq!(p.qualifier, Some("ext.dating.example.*"));

        for bad in [
            "fauna:",
            "fauna:feed",
            "fauna:feed:",
            "fauna::read",
            "fauna:Feed:read",
            "fauna:feed:READ",
            "fauna:feed1:read",
            "fauna:feed:read:",
            "fauna:feed:read:Abc",
            "fauna:feed:read:a:b",
            "fauna:feed:read:a b",
            "fauna:folder:read:abc.*",
            "fauna:records:rw:.*",
            "fauna:records:rw:a.*.*",
            "fauna:records:rw:a*",
            "faunax:feed:read",
            "feed:read",
        ] {
            assert!(parse(bad).is_none(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn the_first_arm_matches_exactly_its_own_string() {
        assert_eq!(arm_of(SCOPE_FEED_READ), Some(FaunaScopeArm::FeedRead));
        for near in [
            "fauna:feed:read:home",
            "fauna:feed:readx",
            "fauna:feed:write",
            "fauna:feeds:read",
            "fauna:feed:read ",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
        }
    }

    /// Every plane the goal doc's table lists but no slice has built matches
    /// no arm — `invalid_scope` at PAR until its own slice adds it.
    #[test]
    fn the_listed_but_unbuilt_planes_match_no_arm() {
        for unbuilt in [
            "fauna:personalization:rw",
            "fauna:post:write",
            "fauna:follow:write",
            "fauna:identity:op:rotate",
        ] {
            assert!(parse(unbuilt).is_some(), "{unbuilt:?} is well-formed");
            assert_eq!(arm_of(unbuilt), None, "{unbuilt:?} is not built");
        }
    }

    #[test]
    fn every_arm_round_trips_through_its_examples() {
        for arm in FaunaScopeArm::ALL {
            assert!(!arm.advertised().is_empty());
            assert!(!arm.examples().is_empty());
            for s in arm.examples() {
                assert_eq!(arm_of(s), Some(*arm), "{s}");
                assert!(card_row(s).is_some(), "{s} renders a card row");
            }
        }
    }

    /// The `records` arm (`third-party-kinds.md` § The record doors): its
    /// qualifier is required and is one `ext.*` kind or one publisher's
    /// wildcard, parsed by the one grammar — a dotted name, a `fauna.` kind,
    /// a bare plane-and-verb or a prefix wildcard matches no arm.
    #[test]
    fn the_records_arm_takes_exactly_one_ext_kind_or_one_publishers_wildcard() {
        use fauna_core::ext_kind::ExtQualifier;
        for good in [
            "fauna:records:rw:ext.app.example.notes",
            "fauna:records:rw:ext.app.example.*",
            "fauna:records:rw:ext.example.co.uk.notes",
        ] {
            assert_eq!(arm_of(good), Some(FaunaScopeArm::Records), "{good}");
            assert!(card_row(good).is_some());
        }
        for bad in [
            SCOPE_RECORDS_RW,
            "fauna:records:rw:notes",
            "fauna:records:rw:fauna.state.notes",
            "fauna:records:rw:ext.notes",
            "fauna:records:rw:ext.-app.example.notes",
            "fauna:records:rw:ext..notes",
            "fauna:records:rw:ext.app.example.*.*",
            "fauna:records:read:ext.app.example.notes",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
            assert_eq!(records_qualifier(bad), None, "{bad}");
        }
        assert_eq!(
            records_qualifier("fauna:records:rw:ext.example.co.*"),
            Some(ExtQualifier::Publisher("example.co".into()))
        );
        let one = records_qualifier("fauna:records:rw:ext.example.co.uk.notes").unwrap();
        assert_eq!(one.publisher(), "example.co.uk");
        assert_eq!(records_qualifier(SCOPE_FEED_READ), None);
        assert!(FaunaScopeArm::Records.has_grant_twin());
    }

    /// The records card row names the publisher and the kind count, says keys
    /// to exactly those records are shared, and says "read only" when the app
    /// attested no writer key; a single foreign kind names its own publisher.
    #[test]
    fn the_records_card_row_names_publisher_count_and_read_only() {
        let wild = "fauna:records:rw:ext.app.example.*";
        let row = records_card_row(wild, Some(2), true).unwrap();
        assert!(row.starts_with("Read and write 2 kinds of records published by app.example"));
        assert!(row.contains("keys to exactly those records are shared"));
        assert!(!row.contains("read only"));

        let ro = records_card_row(wild, Some(1), false).unwrap();
        assert!(
            ro.contains("1 kind of records published by app.example"),
            "{ro}"
        );
        assert!(ro.contains("read only"), "{ro}");

        let unknown = records_card_row(wild, None, true).unwrap();
        assert!(
            unknown.contains("the records published by app.example"),
            "{unknown}"
        );

        let foreign =
            records_card_row("fauna:records:rw:ext.other.org.thing", Some(1), true).unwrap();
        assert!(
            foreign.contains("records of the kind \"thing\" published by other.org"),
            "{foreign}"
        );

        assert_eq!(records_card_row(SCOPE_FEED_READ, Some(1), true), None);
        assert_eq!(records_card_row("atproto", None, true), None);
    }

    /// The `folder_deposit` arm (`file-sync.md` § Third-party deposit
    /// ingress): one folder by its canonical decimal row id, nothing else.
    #[test]
    fn the_folder_deposit_arm_takes_exactly_one_canonical_folder_id() {
        for (good, id) in [
            ("fauna:folder:deposit:42", 42),
            ("fauna:folder:deposit:1", 1),
            ("fauna:folder:deposit:9223372036854775807", i64::MAX),
        ] {
            assert_eq!(arm_of(good), Some(FaunaScopeArm::FolderDeposit), "{good}");
            assert_eq!(folder_deposit_qualifier(good), Some(id), "{good}");
            assert!(card_row(good).is_some_and(|r| r.contains("no keys are shared")));
        }
        for bad in [
            SCOPE_FOLDER_DEPOSIT,
            "fauna:folder:deposit:abc",
            "fauna:folder:deposit:042",
            "fauna:folder:deposit:0",
            "fauna:folder:deposit:-4",
            "fauna:folder:deposit:9223372036854775808",
            "fauna:folder:write:42",
            "fauna:folders:deposit:42",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
            assert_eq!(folder_deposit_qualifier(bad), None, "{bad}");
        }
        // The read arm shares the qualifier, never the reach.
        assert_eq!(folder_deposit_qualifier("fauna:folder:read:42"), None);
        assert_eq!(folder_deposit_qualifier("fauna:records:rw:ext.a.b"), None);
        assert!(FaunaScopeArm::FolderDeposit.has_grant_twin());
    }

    /// The `folder_read` arm (`webdav-server.md` § Key model → *A principal's
    /// read*): one folder by its canonical decimal row id, a keyed twin, the
    /// MDA's door, and refused to a remote-form client — bare or qualified.
    #[test]
    fn the_folder_read_arm_takes_exactly_one_canonical_folder_id() {
        for (good, id) in [
            ("fauna:folder:read:42", 42),
            ("fauna:folder:read:1", 1),
            ("fauna:folder:read:9223372036854775807", i64::MAX),
        ] {
            assert_eq!(arm_of(good), Some(FaunaScopeArm::FolderRead), "{good}");
            assert_eq!(folder_read_qualifier(good), Some(id), "{good}");
            assert_eq!(folder_deposit_qualifier(good), None, "{good}");
            assert!(card_row(good).is_some_and(|r| r.contains("keys to exactly that folder")));
            assert!(refused_to_remote_form(good), "{good}");
        }
        for bad in [
            SCOPE_FOLDER_READ,
            "fauna:folder:read:abc",
            "fauna:folder:read:042",
            "fauna:folder:read:0",
            "fauna:folder:read:-4",
            "fauna:folder:read:9223372036854775808",
            "fauna:folder:list:42",
            "fauna:folders:read:42",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
            assert_eq!(folder_read_qualifier(bad), None, "{bad}");
        }
        assert!(FaunaScopeArm::FolderRead.has_grant_twin());
        assert_eq!(
            FaunaScopeArm::FolderRead.door(),
            Door::Mda {
                relay_kind: WEBDAV_ADMIT_PRINCIPAL_KIND
            }
        );
        // The bare form is refused to a remote client too (a card-qualified
        // request); nothing else is form-gated.
        assert!(refused_to_remote_form(SCOPE_FOLDER_READ));
        for open in [
            SCOPE_FEED_READ,
            "fauna:folder:deposit:42",
            SCOPE_FOLDER_DEPOSIT,
            "fauna:records:rw:ext.app.example.*",
            "fauna:folder:read:abc",
            "atproto",
        ] {
            assert!(!refused_to_remote_form(open), "{open}");
        }
    }

    /// Only the read arm is bridge-doored; every other arm is the nest's.
    #[test]
    fn exactly_one_door_per_arm() {
        for arm in FaunaScopeArm::ALL {
            let mda = matches!(arm.door(), Door::Mda { .. });
            assert_eq!(mda, *arm == FaunaScopeArm::FolderRead, "{arm:?}");
        }
    }

    /// The read card row names the folder when the app knows it, and falls
    /// back to the generic row when it does not.
    #[test]
    fn the_folder_read_card_row_names_the_folder_when_known() {
        let s = "fauna:folder:read:42";
        let named = folder_read_card_row(s, Some("Photos")).unwrap();
        assert!(named.contains("your folder \"Photos\""), "{named}");
        assert!(named.contains("keys to exactly that folder are shared"));
        assert_eq!(folder_read_card_row(s, None).as_deref(), card_row(s));
        assert_eq!(
            folder_read_card_row("fauna:folder:deposit:42", Some("x")),
            None
        );
        assert_eq!(folder_read_card_row(SCOPE_FOLDER_READ, None), None);
    }

    /// The folder plane's qualifier is the user's (`authorization-server.md`
    /// § Scope grammar → *The folder plane's qualifier is the user's*): the
    /// bare string is a card-qualified request — no arm grants it as written,
    /// its bare form is itself, and the user's choice qualifies it into the
    /// arm's string. Every other arm's qualifier is the client's or the
    /// server's, and has no bare form.
    #[test]
    fn the_folder_plane_qualifier_is_the_users() {
        for arm in FaunaScopeArm::ALL {
            let folder = matches!(
                arm,
                FaunaScopeArm::FolderDeposit | FaunaScopeArm::FolderRead
            );
            assert_eq!(arm.qualifier_is_users(), folder, "{arm:?}");
        }
        assert_eq!(
            user_qualified_bare_arm(SCOPE_FOLDER_READ),
            Some(FaunaScopeArm::FolderRead)
        );
        assert_eq!(bare_form("fauna:folder:read:42"), Some(SCOPE_FOLDER_READ));
        assert_eq!(
            qualify_bare_scopes(&[SCOPE_FOLDER_DEPOSIT, SCOPE_FOLDER_READ], 42),
            Some(vec![
                "fauna:folder:deposit:42".to_string(),
                "fauna:folder:read:42".to_string()
            ]),
            "one choice qualifies every bare folder verb"
        );

        assert_eq!(arm_of(SCOPE_FOLDER_DEPOSIT), None);
        assert_eq!(
            user_qualified_bare_arm(SCOPE_FOLDER_DEPOSIT),
            Some(FaunaScopeArm::FolderDeposit)
        );
        assert_eq!(bare_form(SCOPE_FOLDER_DEPOSIT), Some(SCOPE_FOLDER_DEPOSIT));
        assert_eq!(
            bare_form("fauna:folder:deposit:42"),
            Some(SCOPE_FOLDER_DEPOSIT)
        );
        assert_eq!(
            card_row(SCOPE_FOLDER_DEPOSIT),
            card_row("fauna:folder:deposit:42")
        );
        assert_eq!(
            qualify_bare(SCOPE_FOLDER_DEPOSIT, 42).as_deref(),
            Some("fauna:folder:deposit:42")
        );
        assert_eq!(qualify_bare(SCOPE_FOLDER_DEPOSIT, 0), None);
        assert_eq!(qualify_bare("fauna:folder:deposit:42", 7), None);
        assert_eq!(
            qualify_bare_scopes(&["fauna:feed:read", SCOPE_FOLDER_DEPOSIT], 42),
            Some(vec![
                "fauna:feed:read".to_string(),
                "fauna:folder:deposit:42".to_string()
            ])
        );
        assert_eq!(
            qualify_bare_scopes(&["fauna:feed:read", "fauna:folder:deposit:7"], 42),
            None,
            "a choice qualifies only a bare scope"
        );
        assert_eq!(qualify_bare_scopes(&[SCOPE_FOLDER_DEPOSIT], -1), None);

        for not_users in [
            SCOPE_RECORDS_RW,
            "fauna:records:rw:ext.app.example.*",
            SCOPE_FEED_READ,
            SCOPE_IDENTITY_OP_NOSTR_SIGN_EVENT,
            "fauna:identity:op",
            "fauna:folder:deposit:abc",
            "fauna:folders:read",
            "atproto",
        ] {
            assert_eq!(user_qualified_bare_arm(not_users), None, "{not_users}");
            assert_eq!(bare_form(not_users), None, "{not_users}");
            assert_eq!(qualify_bare(not_users, 42), None, "{not_users}");
        }
    }

    /// The oracle arm advertises exactly one string per built class, each
    /// rendering that class's own card row.
    #[test]
    fn the_identity_op_arm_advertises_every_built_class_and_nothing_else() {
        use fauna_core::identity_op::IdentityOpClass;
        let mut want: Vec<String> = IdentityOpClass::ALL
            .iter()
            .map(|c| format!("fauna:identity:op:{}", c.name()))
            .collect();
        let mut got: Vec<String> = FaunaScopeArm::IdentityOp
            .advertised()
            .iter()
            .map(|s| s.to_string())
            .collect();
        want.sort();
        got.sort();
        assert_eq!(got, want);
        for class in IdentityOpClass::ALL {
            let s = format!("fauna:identity:op:{}", class.name());
            assert_eq!(card_row(&s), Some(class.card_row()));
        }
        assert!(FaunaScopeArm::IdentityOp.has_grant_twin());
    }

    /// The sovereign deny list binds at the grammar: no sovereign operation's
    /// name matches the arm, nor an unknown class, nor the bare arm.
    #[test]
    fn no_sovereign_or_unknown_class_matches_the_identity_op_arm() {
        use fauna_core::identity_op::SovereignOp;
        for op in SovereignOp::ALL {
            let s = format!("fauna:identity:op:{}", op.name());
            assert!(parse(&s).is_some(), "{s} is well-formed");
            assert_eq!(arm_of(&s), None, "{s} is sovereign");
            assert_eq!(card_row(&s), None, "{s} renders no card row");
        }
        for bad in [
            "fauna:identity:op",
            "fauna:identity:op:nostr",
            "fauna:identity:op:nostr.nip04",
            "fauna:identity:op:atproto.commit",
            "fauna:identity:read:nostr.sign_event",
        ] {
            assert_eq!(arm_of(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_events_arm_takes_no_qualifier_and_shares_no_keys() {
        assert_eq!(
            arm_of(SCOPE_EVENTS_SUBSCRIBE),
            Some(FaunaScopeArm::EventsSubscribe)
        );
        assert!(!FaunaScopeArm::EventsSubscribe.has_grant_twin());
        for near in [
            "fauna:events:subscribe:ext.a.example.notes",
            "fauna:events:read",
            "fauna:event:subscribe",
        ] {
            assert_eq!(arm_of(near), None, "{near:?} is not the arm");
        }
        assert!(card_row(SCOPE_EVENTS_SUBSCRIBE).is_some());
    }

    /// The filter both events doors apply: the events arm plus a `records`
    /// qualifier covering the tag's kind — structurally, by publisher.
    #[test]
    fn an_event_reaches_only_a_scope_the_session_can_list() {
        let both = vec![
            SCOPE_EVENTS_SUBSCRIBE.to_string(),
            "fauna:records:rw:ext.a.example.*".to_string(),
        ];
        assert!(event_reaches(&both, "ext:ext.a.example.notes"));
        // Another publisher's kind, a first-party scope, a folder name and a
        // non-canonical spelling reach nothing.
        for other in [
            "ext:ext.b.example.notes",
            "state",
            "state-fleet",
            "__ext:ext.a.example.notes",
            "ext:EXT.a.example.notes",
            "",
        ] {
            assert!(!event_reaches(&both, other), "{other:?} must not reach");
        }
        // The records arm alone subscribes to nothing; the events arm alone
        // covers no scope.
        let records_only = vec!["fauna:records:rw:ext.a.example.*".to_string()];
        assert!(!event_reaches(&records_only, "ext:ext.a.example.notes"));
        let events_only = vec![SCOPE_EVENTS_SUBSCRIBE.to_string()];
        assert!(!event_reaches(&events_only, "ext:ext.a.example.notes"));
        // An exact-kind qualifier covers exactly its kind.
        let exact = vec![
            SCOPE_EVENTS_SUBSCRIBE.to_string(),
            "fauna:records:rw:ext.a.example.notes".to_string(),
        ];
        assert!(event_reaches(&exact, "ext:ext.a.example.notes"));
        assert!(!event_reaches(&exact, "ext:ext.a.example.todo"));
    }
}
