//! Scope strings — the canonical name of an account-plane scope.
//!
//! Owner: `docs/goal/architecture/account-sync-plane.md` § Feeds and cursors →
//! *Scope partition* → *The scope string*. A scope's name is one canonical
//! string, and that one string is three things at once: the store's at-rest
//! key (every journal, frontier, state-entry and adopted-segment row), the
//! feed request's `scope`, and the vocabulary admission verdicts and custody
//! grants enumerate (charter § The admission seam). The grammar is
//! family-first — the first `:`-segment names the family, and each family
//! owns its remainder:
//!
//! - [`ACCOUNT_STATE_SCOPE`](crate::account_state::ACCOUNT_STATE_SCOPE)
//!   (`"state"`) — the account-state scope, the whole string.
//! - `content:<kind>:<scope-id-hex>` — a content scope, one per
//!   `(kind, scope_id)`: the kind tag **verbatim** from the segment-store
//!   kind table (`message-segment-store.md` § Layout is the only authority —
//!   never the `__`-prefixed directory name derived from it) and the
//!   32-byte scope id as exactly 64 lowercase hex chars (the owner/recipient/
//!   author actor for the own-actor kinds; the MLS channel for `conv`).
//! - `folder:<channel-id-hex>` — a shared file set's scope, one per set,
//!   named by the set's derived MLS `ChannelId` as exactly 64 lowercase hex
//!   chars (ruled by the W8 (account-data-plane.md § Workstreams) share-twin build, 2026-08-17 —
//!   `docs/goal/behavior/p2p.md` § Cross-user shared-set transfer → *Build
//!   contract*; the family was reserved at W2). A folder scope is a SHARED
//!   plane: the wide own-account verdict forms never admit it (see
//!   [`AdmittedScopes::admits`]) — its admission doors are the explicit
//!   `Named` list (the M2-membership witness's product) only. T17's
//!   materialization-grant scope will take its own family tag too — neither
//!   ever lands under `content`.
//! - `group:<scope-id-hex>` — a **storage group** scope under the T20
//!   recipient-set scheme, one per group, named by the content-derived scope
//!   id its birth record commits to (`fauna_core::group_scope`), as exactly
//!   64 lowercase hex chars. Like a folder scope it is a SHARED plane —
//!   owned by its roster, never by an account — so neither wide
//!   [`AdmittedScopes`] form admits one; its door is the explicit `Named`
//!   list, whose producer here is the membership witness (the admission
//!   seam's fourth kind, `account-data-plane.md` § The admission seam). The
//!   family-level refusal is deliberately the whole answer: the co-authored
//!   *content-kind* blocklist below is content-family vocabulary and never
//!   sees this family.
//! - `ext:<kind>` — a **third-party kind's** scope, one per `ext.*` kind, the
//!   kind string verbatim after the family tag (`ext:ext.example.com.notes`;
//!   owner `third-party-kinds.md` § The `ext` sub-scope). An own-account,
//!   delegable-rung scope — both wide [`AdmittedScopes`] forms admit it, it
//!   is never co-authored — kept per kind so a single-kind grantee never
//!   observes a sibling kind's churn.
//!
//! Two of the owner's rulings shape this API (the rest live with the owner):
//!
//! - **Absolute, never store-relative.** The string always carries the scope
//!   id, own-actor scopes included: a `conv` channel's member replicas belong
//!   to *different accounts*, and admission's set check and the frontier
//!   exchange compare scope names by string equality across those stores — a
//!   "my mail" spelling has no meaning there. There is only
//!   `content:mail:<recipient-hex>`, and a member scope of someone else's
//!   content is the same string the owner's own replica uses.
//! - **One spelling, refused not repaired.** [`Scope`]'s parse is strict
//!   (lowercase hex, exact segment count, well-formed kind) and never
//!   normalizes — a normalizing parser would admit two spellings of one
//!   scope past the string-equality checks the canonical form exists to make
//!   trivial. Parsing checks *shape*, not kind knowledge: "well-formed" and
//!   "known to this binary" are separate questions (the
//!   [`ItemClass::from_wire`](crate::account_state::ItemClass::from_wire)
//!   compat posture), so a newer kind's scope survives an older binary's
//!   hands in a grant or a stored row. An unknown *family* is a parse error;
//!   a consumer that only carries a scope onward keeps the raw string.

use std::fmt;
use std::str::FromStr;

use crate::account_state::ACCOUNT_STATE_SCOPE;
use crate::ext_kind::ExtKind;

/// The content-scope family tag — the first `:`-segment of every content
/// scope string.
pub const CONTENT_FAMILY: &str = "content";

/// The folder-scope family tag — the first `:`-segment of every shared-set
/// scope string (module header: ruled 2026-08-17 by the W8 share-twin build).
pub const FOLDER_FAMILY: &str = "folder";

/// The group-scope family tag — the first `:`-segment of every storage-group
/// scope string (module header: the T20 recipient-set scheme, row 62).
pub const GROUP_FAMILY: &str = "group";

/// The third-party-kind family tag — the first `:`-segment of every `ext.*`
/// kind's scope (module header; owner `third-party-kinds.md` § The `ext`
/// sub-scope).
pub const EXT_FAMILY: &str = "ext";

/// Content-scope kinds whose plane other accounts co-author — today exactly
/// the `conv` channels (the scope id is the MLS channel, and every member's
/// replica carries the shared plane).
///
/// This is the one owner of the shared-audience carve-out's vocabulary
/// (`account-data-plane.md` § Replica posture → *The custody grant +
/// ceremony*): a custody grant's `Account` form covers the owner's
/// single-principal scope set — current AND future — so the carve-out is a
/// **blocklist, deliberately**: a future single-principal kind is covered
/// automatically (the no-silent-decay register constraint), while a future
/// co-authored kind must be added here in the same change that introduces
/// it — which the T20 gate guarantees, since the first group scope must
/// answer group-side consent before its plane rides cross-account custody at
/// all.
pub const CO_AUTHORED_CONTENT_KINDS: &[&str] = &["conv"];

/// Is `scope` a canonical scope string of a co-authored plane (see
/// [`CO_AUTHORED_CONTENT_KINDS`])? The account-state scopes and every
/// single-principal content scope answer `false`; so does any string that is
/// not a well-formed scope (shape refusal is the admission check's own job —
/// this predicate answers only "known co-authored").
pub fn is_co_authored_scope(scope: &str) -> bool {
    match scope.parse::<Scope>() {
        Ok(Scope::Content(c)) => CO_AUTHORED_CONTENT_KINDS.contains(&c.kind()),
        _ => false,
    }
}

/// Which scopes an admission verdict admits — the scope-predicate half of
/// the admission seam's verdict (`account-data-plane.md` § The peer leg →
/// *The admission seam*), lifted here (W8.6) so the peer leg's serve side
/// and the nest custody door evaluate ONE vocabulary. The verdict's other
/// halves (account, validity bound) stay with their evaluators; this enum
/// answers only "does the admitted set cover this scope string".
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdmittedScopes {
    /// Every scope of the verdict's account — the `DeviceAuthorization`
    /// verdict ("all scopes of the named account").
    AllOfAccount,
    /// The account's whole **single-principal** scope set — the custody
    /// grant's `Account` form (T13): everything [`Self::AllOfAccount`]
    /// admits EXCEPT co-authored planes (the shared-audience carve-out,
    /// [`is_co_authored_scope`]). A predicate, never a set frozen at mint —
    /// a scope joined tomorrow is covered with no re-mint ("coverage must
    /// not silently decay as the account's scope set grows").
    AllOfAccountSinglePrincipal,
    /// Exactly the named scopes — the shape the M2-membership witness and
    /// the custody grant's explicit-list form produce.
    Named(Vec<String>),
}

impl AdmittedScopes {
    /// Does the admitted set cover `scope`? Shape-checks the string on the
    /// all-of-account forms so an arbitrary string can never ride a wide
    /// verdict into a store layer: an account-state scope
    /// ([`crate::account_state::is_served_scope`] — BOTH halves of the A5
    /// partition) or a well-formed content scope. A **folder** scope is a
    /// shared plane and belongs to its roster, not to an account, so neither
    /// wide form ever admits one — its only door is the explicit `Named`
    /// list (the co-authored carve-out's reasoning, one family out; module
    /// header owns the ruling).
    pub fn admits(&self, scope: &str) -> bool {
        match self {
            AdmittedScopes::AllOfAccount => is_own_account_scope(scope),
            AdmittedScopes::AllOfAccountSinglePrincipal => {
                is_own_account_scope(scope) && !is_co_authored_scope(scope)
            }
            AdmittedScopes::Named(named) => named.iter().any(|s| s == scope),
        }
    }
}

/// Is `scope` one an *own-account* verdict can cover — an account-state
/// scope (both A5-partition halves), a well-formed **content** scope, or a
/// well-formed **`ext`** scope (one account's data however many writers sign
/// into it)? The folder and group families answer `false` by construction:
/// shared planes are admitted per-set, never by account width.
pub fn is_own_account_scope(scope: &str) -> bool {
    crate::account_state::is_served_scope(scope)
        || matches!(
            scope.parse::<Scope>(),
            Ok(Scope::Content(_) | Scope::Ext(_))
        )
}

/// The `ext.*` kind `scope` names, when it is a well-formed `ext` scope.
pub fn ext_scope_kind(scope: &str) -> Option<ExtKind> {
    match scope.parse::<Scope>() {
        Ok(Scope::Ext(kind)) => Some(kind),
        _ => None,
    }
}

/// The canonical `ext` scope string of `kind` (`ext:<kind>`).
pub fn ext_scope(kind: &ExtKind) -> String {
    Scope::Ext(kind.clone()).to_string()
}

/// A parsed scope string. Constructing one (or parsing one) is the only door,
/// so a value of this type is canonical by construction and its `Display` is
/// the canonical spelling.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Scope {
    /// The account-state scope (`"state"`) — exactly one per account.
    AccountState,
    /// A content scope — one per `(kind, scope_id)`.
    Content(ContentScope),
    /// A shared file set's scope — one per set, named by its `ChannelId`
    /// (module header: the folder family's ruling).
    Folder(FolderScope),
    /// A storage group's scope — one per T20 recipient-set group, named by
    /// its birth record's content-derived scope id (module header).
    Group(GroupScope),
    /// A third-party kind's scope — one per `ext.*` kind (module header).
    Ext(ExtKind),
}

/// A content scope's `(kind, scope_id)` pair.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ContentScope {
    kind: String,
    scope_id: [u8; 32],
}

/// A folder scope's identity — the shared set's derived MLS `ChannelId`
/// (the same 32 bytes that key the set's group, custody envelope, and
/// `fauna.folders.*` wire everywhere else).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FolderScope {
    channel_id: [u8; 32],
}

impl FolderScope {
    /// Build the scope for one shared set. Infallible: every 32-byte channel
    /// id names a well-formed folder scope.
    pub fn new(channel_id: [u8; 32]) -> Self {
        Self { channel_id }
    }

    /// The set's 32-byte derived channel id.
    pub fn channel_id(&self) -> &[u8; 32] {
        &self.channel_id
    }
}

impl fmt::Display for FolderScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{FOLDER_FAMILY}:{}", hex::encode(self.channel_id))
    }
}

/// A group scope's identity — the 32-byte **content-derived scope id** its
/// birth record commits to (`fauna_core::group_scope::group_scope_id`, the
/// R14 (account-data-plane.md § The ratified decisions) key↔id commitment pattern). Nothing here re-derives it: this type
/// names a scope, and whether a given id is the honest hash of a birth record
/// the *reader* holds is that reader's check, exactly as the mint resolver
/// re-derives a generation id before trusting its row.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct GroupScope {
    scope_id: [u8; 32],
}

impl GroupScope {
    /// Build the scope for one storage group. Infallible: every 32-byte scope
    /// id names a well-formed group scope.
    pub fn new(scope_id: [u8; 32]) -> Self {
        Self { scope_id }
    }

    /// The group's 32-byte content-derived scope id.
    pub fn scope_id(&self) -> &[u8; 32] {
        &self.scope_id
    }
}

impl fmt::Display for GroupScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{GROUP_FAMILY}:{}", hex::encode(self.scope_id))
    }
}

/// Why a scope string (or kind tag) was refused. Refusals are deliberate
/// where repair would be possible — see the module header.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ScopeError {
    /// The first `:`-segment names no family this binary knows.
    #[error("unknown scope family in {0:?}")]
    UnknownFamily(String),
    /// The family is known but the segment count is not its grammar's —
    /// notably the pre-ruling `content:<kind>` spelling with no scope id.
    #[error("wrong shape for scope {0:?}: {1}")]
    BadShape(String, &'static str),
    /// A kind tag is `[a-z][a-z0-9-]*` — never empty, never uppercase, never
    /// the `__`-prefixed *directory* name the segment store derives from it.
    #[error("malformed kind tag {0:?}")]
    BadKind(String),
    /// A scope id is exactly 64 lowercase hex chars (32 bytes).
    #[error("malformed scope id {0:?}")]
    BadScopeId(String),
    /// An `ext` scope's remainder is not an `ext.*` kind (§ The grammar).
    #[error("malformed ext scope {0:?}: {1}")]
    BadExtKind(String, crate::ext_kind::ExtKindError),
}

fn valid_kind_tag(tag: &str) -> bool {
    let mut bytes = tag.bytes();
    matches!(bytes.next(), Some(b'a'..=b'z'))
        && bytes.all(|b| matches!(b, b'a'..=b'z' | b'0'..=b'9' | b'-'))
}

impl ContentScope {
    /// Build the scope for one `(kind, scope_id)`. The kind tag is validated
    /// in shape only (see the module header); whether a given nest *serves*
    /// the kind is that nest's door to answer.
    pub fn new(kind: &str, scope_id: [u8; 32]) -> Result<Self, ScopeError> {
        if !valid_kind_tag(kind) {
            return Err(ScopeError::BadKind(kind.to_string()));
        }
        Ok(Self {
            kind: kind.to_string(),
            scope_id,
        })
    }

    /// The segment-store kind tag (`message-segment-store.md` § Layout).
    pub fn kind(&self) -> &str {
        &self.kind
    }

    /// The 32-byte scope id the kind table assigns the kind.
    pub fn scope_id(&self) -> &[u8; 32] {
        &self.scope_id
    }
}

impl fmt::Display for ContentScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{CONTENT_FAMILY}:{}:{}",
            self.kind,
            hex::encode(self.scope_id)
        )
    }
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::AccountState => f.write_str(ACCOUNT_STATE_SCOPE),
            Self::Content(c) => c.fmt(f),
            Self::Folder(fs) => fs.fmt(f),
            Self::Group(gs) => gs.fmt(f),
            Self::Ext(k) => write!(f, "{EXT_FAMILY}:{k}"),
        }
    }
}

/// Parse the single-hex-id remainder shared by the `folder` and `group`
/// families: exactly one segment of 64 lowercase hex chars. Strict, never
/// repairing — uppercase hex spells a *different string* for the same id,
/// which equality-compared consumers must never meet.
fn parse_hex32_tail(
    s: &str,
    parts: &mut std::str::Split<'_, char>,
    shape: &'static str,
) -> Result<[u8; 32], ScopeError> {
    let (Some(id_hex), None) = (parts.next(), parts.next()) else {
        return Err(ScopeError::BadShape(s.to_string(), shape));
    };
    if !fauna_core::hex32::is_lowercase_hex64(id_hex) {
        return Err(ScopeError::BadScopeId(id_hex.to_string()));
    }
    let mut id = [0u8; 32];
    hex::decode_to_slice(id_hex, &mut id)
        .map_err(|_| ScopeError::BadScopeId(id_hex.to_string()))?;
    Ok(id)
}

impl FromStr for Scope {
    type Err = ScopeError;

    fn from_str(s: &str) -> Result<Self, ScopeError> {
        if s == ACCOUNT_STATE_SCOPE {
            return Ok(Self::AccountState);
        }
        let mut parts = s.split(':');
        match parts.next().unwrap_or("") {
            ACCOUNT_STATE_SCOPE => Err(ScopeError::BadShape(
                s.to_string(),
                "the account-state scope is the whole string, no further segments",
            )),
            CONTENT_FAMILY => {
                let (Some(kind), Some(id_hex), None) = (parts.next(), parts.next(), parts.next())
                else {
                    return Err(ScopeError::BadShape(
                        s.to_string(),
                        "a content scope is exactly content:<kind>:<scope-id-hex>",
                    ));
                };
                if !valid_kind_tag(kind) {
                    return Err(ScopeError::BadKind(kind.to_string()));
                }
                // Strict, not repairing: uppercase hex spells a *different
                // string* for the same id, which equality-compared consumers
                // must never meet.
                if !fauna_core::hex32::is_lowercase_hex64(id_hex) {
                    return Err(ScopeError::BadScopeId(id_hex.to_string()));
                }
                let mut scope_id = [0u8; 32];
                hex::decode_to_slice(id_hex, &mut scope_id)
                    .map_err(|_| ScopeError::BadScopeId(id_hex.to_string()))?;
                Ok(Self::Content(ContentScope {
                    kind: kind.to_string(),
                    scope_id,
                }))
            }
            FOLDER_FAMILY => Ok(Self::Folder(FolderScope {
                channel_id: parse_hex32_tail(
                    s,
                    &mut parts,
                    "a folder scope is exactly folder:<channel-id-hex>",
                )?,
            })),
            GROUP_FAMILY => Ok(Self::Group(GroupScope {
                scope_id: parse_hex32_tail(
                    s,
                    &mut parts,
                    "a group scope is exactly group:<scope-id-hex>",
                )?,
            })),
            EXT_FAMILY => {
                // The kind contains no `:`, so the remainder is exactly one
                // segment; `ExtKind`'s parse is the one door.
                let (Some(rest), None) = (parts.next(), parts.next()) else {
                    return Err(ScopeError::BadShape(
                        s.to_string(),
                        "an ext scope is exactly ext:<ext.* kind>",
                    ));
                };
                rest.parse::<ExtKind>()
                    .map(Self::Ext)
                    .map_err(|e| ScopeError::BadExtKind(s.to_string(), e))
            }
            _ => Err(ScopeError::UnknownFamily(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(b: u8) -> [u8; 32] {
        [b; 32]
    }

    #[test]
    fn account_state_scope_round_trips() {
        let parsed: Scope = ACCOUNT_STATE_SCOPE.parse().unwrap();
        assert_eq!(parsed, Scope::AccountState);
        assert_eq!(parsed.to_string(), ACCOUNT_STATE_SCOPE);
    }

    #[test]
    fn content_scope_round_trips_at_exactly_the_canonical_spelling() {
        let scope = ContentScope::new("post", id(0xC3)).unwrap();
        let text = Scope::Content(scope.clone()).to_string();
        assert_eq!(text, format!("content:post:{}", "c3".repeat(32)));
        assert_eq!(text.parse::<Scope>().unwrap(), Scope::Content(scope));
    }

    /// Regression pins on the two ad-hoc spellings the pre-ruling fixtures
    /// used — each must be a refusal, so neither can quietly become a scope.
    #[test]
    fn the_pre_ruling_fixture_spellings_are_refused() {
        // `content:post` (conformance_account_bootstrap.rs): no scope id —
        // the relative spelling the absoluteness ruling forbids.
        assert!(matches!(
            "content:post".parse::<Scope>(),
            Err(ScopeError::BadShape(..))
        ));
        // `content:__post:` (store.rs): the *directory* name, not the kind tag.
        let dir_spelling = format!("content:__post:{}", "0d".repeat(32));
        assert!(matches!(
            dir_spelling.parse::<Scope>(),
            Err(ScopeError::BadKind(..))
        ));
    }

    #[test]
    fn parse_refuses_rather_than_normalizes() {
        let cases: &[(String, &str)] = &[
            (format!("content:post:{}", "C3".repeat(32)), "uppercase hex"),
            (format!("content:post:{}", "c3".repeat(31)), "short id"),
            (format!("content:post:{}x", "c3".repeat(32)), "long id"),
            (format!("content:post:{}", "zz".repeat(32)), "non-hex id"),
            (format!("content::{}", "c3".repeat(32)), "empty kind"),
            (
                format!("content:Post:{}", "c3".repeat(32)),
                "uppercase kind",
            ),
            (
                format!("content:post:{}:extra", "c3".repeat(32)),
                "trailing segment",
            ),
        ];
        for (text, why) in cases {
            assert!(
                text.parse::<Scope>().is_err(),
                "must refuse {why}: {text:?}"
            );
        }
    }

    #[test]
    fn unknown_family_is_an_error_not_a_guess() {
        for text in [
            format!("posts:{}", "c3".repeat(32)), // no such family ("post" is a kind)
            String::new(),
        ] {
            assert!(matches!(
                text.parse::<Scope>(),
                Err(ScopeError::UnknownFamily(_))
            ));
        }
        assert!(matches!(
            "state:extra".parse::<Scope>(),
            Err(ScopeError::BadShape(..))
        ));
    }

    // ── the folder family (ruled by the W8 share-twin build, 2026-08-17) ─────

    #[test]
    fn folder_scope_round_trips_at_exactly_the_canonical_spelling() {
        let scope = FolderScope::new(id(0x4F));
        let text = Scope::Folder(scope.clone()).to_string();
        assert_eq!(text, format!("folder:{}", "4f".repeat(32)));
        assert_eq!(text.parse::<Scope>().unwrap(), Scope::Folder(scope));
    }

    #[test]
    fn folder_parse_refuses_rather_than_normalizes() {
        let cases: &[(String, &str)] = &[
            (format!("folder:{}", "4F".repeat(32)), "uppercase hex"),
            (format!("folder:{}", "4f".repeat(31)), "short id"),
            (format!("folder:{}x", "4f".repeat(32)), "long id"),
            (format!("folder:{}", "zz".repeat(32)), "non-hex id"),
            ("folder".to_string(), "no channel id"),
            ("folder:".to_string(), "empty channel id"),
            (
                format!("folder:{}:extra", "4f".repeat(32)),
                "trailing segment",
            ),
        ];
        for (text, why) in cases {
            assert!(
                text.parse::<Scope>().is_err(),
                "must refuse {why}: {text:?}"
            );
        }
    }

    /// A folder scope is a SHARED plane — its only admission door is the
    /// explicit `Named` form (the M2-membership witness's product, and the
    /// custody grant's explicit-list form). The wide own-account verdicts
    /// never cover it: `AllOfAccount` answers "every scope of the named
    /// account", and a shared set belongs to its roster, not to an account —
    /// the same reasoning that keeps co-authored `conv` planes off the
    /// `Account`-form custody verdict, applied one family out.
    #[test]
    fn the_wide_own_account_verdicts_never_admit_a_folder_scope() {
        let folder = format!("folder:{}", "4f".repeat(32));
        assert!(!AdmittedScopes::AllOfAccount.admits(&folder));
        assert!(!AdmittedScopes::AllOfAccountSinglePrincipal.admits(&folder));
        assert!(AdmittedScopes::Named(vec![folder.clone()]).admits(&folder));
    }

    #[test]
    fn constructor_holds_the_same_kind_door_as_parse() {
        for bad in ["__mail", "", "Post", "po st", "po:st"] {
            assert!(matches!(
                ContentScope::new(bad, id(1)),
                Err(ScopeError::BadKind(_))
            ));
        }
        for good in ["mail", "conv", "calendar", "card", "post", "kind-2"] {
            assert!(ContentScope::new(good, id(1)).is_ok());
        }
    }

    #[test]
    fn co_authored_is_exactly_the_conv_family_today() {
        assert!(is_co_authored_scope(&format!(
            "content:conv:{}",
            "2b".repeat(32)
        )));
        // Single-principal content, the account-state scopes, and malformed
        // strings are all "not co-authored" — the predicate never widens on
        // shape refusals (shape is the admission check's own job).
        for not in [
            format!("content:mail:{}", "2b".repeat(32)),
            format!("content:post:{}", "2b".repeat(32)),
            "state".to_string(),
            "state-fleet".to_string(),
            format!("content:conv:{}", "2B".repeat(32)), // non-canonical spelling
            "___not_a_scope".to_string(),
        ] {
            assert!(!is_co_authored_scope(&not), "must not flag {not:?}");
        }
    }

    // ── the group family (T20 recipient-set scheme, row 62 slice 1a) ────────

    #[test]
    fn group_scope_round_trips_at_exactly_the_canonical_spelling() {
        let scope = GroupScope::new(id(0x7A));
        let text = Scope::Group(scope.clone()).to_string();
        assert_eq!(text, format!("group:{}", "7a".repeat(32)));
        assert_eq!(text.parse::<Scope>().unwrap(), Scope::Group(scope));
    }

    #[test]
    fn group_parse_refuses_rather_than_normalizes() {
        let cases: &[(String, &str)] = &[
            (format!("group:{}", "7A".repeat(32)), "uppercase hex"),
            (format!("group:{}", "7a".repeat(31)), "short id"),
            (format!("group:{}x", "7a".repeat(32)), "long id"),
            (format!("group:{}", "zz".repeat(32)), "non-hex id"),
            ("group".to_string(), "no scope id"),
            ("group:".to_string(), "empty scope id"),
            (
                format!("group:{}:extra", "7a".repeat(32)),
                "trailing segment",
            ),
        ];
        for (text, why) in cases {
            assert!(
                text.parse::<Scope>().is_err(),
                "must refuse {why}: {text:?}"
            );
        }
    }

    /// A group scope is a SHARED plane owned by its roster, exactly like a
    /// folder scope — so neither wide own-account verdict admits one, and its
    /// only door is the explicit `Named` list (the membership witness's
    /// product). The folder family's reasoning, one family out.
    #[test]
    fn the_wide_own_account_verdicts_never_admit_a_group_scope() {
        let group = format!("group:{}", "7a".repeat(32));
        assert!(!AdmittedScopes::AllOfAccount.admits(&group));
        assert!(!AdmittedScopes::AllOfAccountSinglePrincipal.admits(&group));
        assert!(AdmittedScopes::Named(vec![group.clone()]).admits(&group));
    }

    /// The co-authored *content-kind* blocklist is a content-family
    /// vocabulary; a group scope is its own family and is refused a rung
    /// wider — by `admits` above, never by this predicate. Pinned so nobody
    /// "fixes" the false answer here and weakens the family-level refusal.
    #[test]
    fn a_group_scope_is_not_a_co_authored_content_kind() {
        assert!(!is_co_authored_scope(&format!("group:{}", "7a".repeat(32))));
    }

    #[test]
    fn parse_checks_shape_not_kind_knowledge() {
        // A newer binary's kind must survive an older binary's parse — the
        // ItemClass::from_wire posture, not a closed-set refusal.
        let text = format!("content:somefuturekind:{}", "ab".repeat(32));
        match text.parse::<Scope>().unwrap() {
            Scope::Content(c) => assert_eq!(c.kind(), "somefuturekind"),
            other => panic!("expected a content scope, got {other:?}"),
        }
    }

    // ── the ext family (third-party-kinds.md § The `ext` sub-scope) ─────────

    #[test]
    fn ext_scope_round_trips_at_exactly_the_canonical_spelling() {
        let kind: ExtKind = "ext.example.com.notes".parse().unwrap();
        let text = ext_scope(&kind);
        assert_eq!(text, "ext:ext.example.com.notes");
        assert_eq!(text.parse::<Scope>().unwrap(), Scope::Ext(kind.clone()));
        assert_eq!(ext_scope_kind(&text), Some(kind));
    }

    #[test]
    fn ext_parse_refuses_rather_than_normalizes() {
        for bad in [
            "ext",
            "ext:",
            "ext:example.com.notes",     // the kind's own tag is required
            "ext:ext.Example.com.notes", // uppercase
            "ext:ext.example.com.notes:extra", // trailing segment
            "ext:ext.example.com.*",     // a wildcard is a qualifier, never a scope
            "ext:fauna.state.profile",   // first-party kinds never take this family
        ] {
            assert!(bad.parse::<Scope>().is_err(), "must refuse {bad:?}");
        }
    }

    /// An `ext` scope is the account's own data on the delegable rung: both
    /// wide verdicts admit it (the account's replicas and custodians hold it
    /// as they hold `state`), and it is never co-authored however many
    /// writers sign into it.
    #[test]
    fn the_wide_own_account_verdicts_admit_an_ext_scope_and_it_is_not_co_authored() {
        let ext = "ext:ext.example.com.notes";
        assert!(is_own_account_scope(ext));
        assert!(AdmittedScopes::AllOfAccount.admits(ext));
        assert!(AdmittedScopes::AllOfAccountSinglePrincipal.admits(ext));
        assert!(!is_co_authored_scope(ext));
        assert!(!is_own_account_scope("ext:ext.example.com.*"));
    }
}
