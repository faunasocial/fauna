//! The publicly-synced follow's two **write** recipes — follow and unfollow —
//! written once and run by every app (`docs/goal/ui/folders.md` § Following a
//! public folder; behavior authority `docs/goal/behavior/folders.md`
//! § Publicly-synced follow).
//!
//! [`public_follow`](crate::public_follow) deliberately holds only the *read*
//! mechanisms ("nothing here writes follow state to any nest"), and
//! `fauna_client_config::save_follow` holds only the persistence (over the
//! account's `FollowsStore` — `fauna.state.follows`). What sat
//! between them — resolve the address a user typed, run the first public fetch,
//! then persist the record it pins — was app glue, and by 2026-08-20 three
//! copies of it existed (tui's `follow_public`, the wasm face's
//! `followPublicFolder`, and the web SPA's own resolve-then-call). This module
//! is that composition, lifted (priority #2) so a leg on the remaining apps is a
//! render, not a fourth re-derivation of the address rules. The wasm face has
//! called it since 2026-09-22 — before that the web SPA resolved the handle
//! itself and always followed on its own nest.
//!
//! **The owner is accepted in either shape**, exactly as `share_set` takes a
//! recipient: a bare 64-hex actor id is classified locally, anything else is a
//! Fauna handle — a bare `alice` or an `alice@domain`. Same superset, same
//! reason — the shared UX names "handle", and an actor id pasted in should not
//! be a dead end.
//!
//! ⚠ **The home nest is DISCOVERED from the handle, never typed.** A handle's
//! `@domain` is the address's nest half, and [`resolve_owner_address`] runs the
//! SAME chain the conversations recipient picker runs
//! (`FaunaMlsBackend::resolve_address`): a same-nest `fauna.actor.by_handle`
//! probe first — which also yields this nest's own handle domain — then the one
//! shared same-nest-vs-cross-nest decision,
//! [`fauna_core::resolve::is_foreign_handle_domain`]. A bare handle, or a
//! domain that is this nest's, is the **same-nest** follow: the record's
//! `home_nest_url` is empty and the follower's own nest serves the folder. A
//! **foreign** domain is resolved directly against that peer through the
//! anonymous discovery hop (`fauna_client_conversations::actor_by_handle_remote`,
//! `federation.md` § Peer-auth model), and the record's `home_nest_url` is the
//! peer's base URL from `peer_nest_url` — the one domain→URL derivation every
//! cross-nest caller shares — so every read after the first is relayed there by
//! the follower's own nest (`fauna.folders.public.fetch{nest_url}`). A bare
//! actor id names no nest and is always same-nest. Neither arm asks the user
//! for a nest url, which is why the flow does not collect one.
//!
//! ⚠ **Absent, private and misspelled fail IDENTICALLY.** The home nest folds
//! the three so nothing can probe for the existence of a sealed folder
//! (`behavior/folders.md` § Publicly-synced follow), so [`FollowOpError`] carries
//! one not-found arm and the caller must not invent a friendlier per-case
//! message — that would hand back exactly the distinction the nest refused to
//! make. A handle no nest knows, and a foreign nest's definite refusal, fold
//! into the same arm for the same reason. The single ratified wording is
//! `devices::ERROR_FOLLOW_NOT_FOUND`, resolved by [`follow_error_text`].
//!
//! Gated behind this crate's `mls` feature only because it reuses
//! [`ConversationsClient::actor_by_handle`] and the anon hop beside it rather
//! than re-issuing `fauna.actor.by_handle` with its own copy of that call's
//! not-found-is-`Ok(None)` mapping. Every app that can follow a folder already
//! links the sharing stack, so the gate costs no consumer anything.

use fauna_client_conversations::ConversationsClient;
use fauna_conversations::backend::ConvRpcError;
use fauna_core::data::FollowedFolder;
use fauna_core::identity::ActorId;
use fauna_core::resolve::{is_foreign_handle_domain, parse_fauna_handle};
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// Why a follow or unfollow could not complete.
///
/// Deliberately coarser than the transport's own errors: the three
/// "no such public folder" causes are folded by the nest and stay folded here.
#[derive(Debug)]
pub enum FollowOpError {
    /// No public folder at that address — absent, private, never declassified,
    /// deleted, or misspelled. **One arm on purpose** (see the module docs).
    NotFound,
    /// Anything else: a transport fault, an unreadable config, a rejected
    /// write. Carries the underlying display string for the page's
    /// `error-message`.
    Failed(String),
}

impl std::fmt::Display for FollowOpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotFound => write!(f, "no public folder at that address"),
            Self::Failed(e) => write!(f, "{e}"),
        }
    }
}

/// The wording half of the split this module's own FOLD backs: linux and tui
/// each hand-carried this identical two-arm mapping (found by the dev-fleet
/// near-duplicate-function scanner's cross-crate pass, 0.66 similarity —
/// same name, same shape, differing only in which re-export path reached
/// `fauna_i18n::strings`). `fauna-i18n` is a zero-dependency generated-string
/// crate several sibling client crates already take unconditionally
/// (`fauna-client-backup` et al.), so there was no wasm-weight reason to keep
/// this app-side once both call sites turned out to resolve the exact same
/// catalog entries with no per-app customization.
pub fn follow_error_text(e: FollowOpError) -> String {
    match e {
        FollowOpError::NotFound => fauna_i18n::strings::devices::ERROR_FOLLOW_NOT_FOUND.to_string(),
        FollowOpError::Failed(msg) => fauna_i18n::strings::devices::error_follow_failed(&msg),
    }
}

/// The owner half of a follow address, resolved: **who**, and **on which
/// nest** — the two things the first public fetch needs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOwner {
    /// Canonical (lowercase) 64-hex actor id.
    pub actor_id: String,
    /// The owner's home nest base URL: **empty** for the caller's own nest (the
    /// same-nest follow — `FollowedFolder::home_nest_url`'s own convention),
    /// else the peer the follower's nest relays every read to.
    pub home_nest_url: String,
}

/// Resolve the owner half of a follow address (the module docs own the chain).
///
/// A bare 64-hex actor id is classified locally and returned as-is
/// (normalized), homed on the caller's nest. A handle that resolves nowhere —
/// unknown here for a bare handle, unknown or definitely refused at the peer
/// for a foreign one — is [`FollowOpError::NotFound`], the same answer a
/// folder that does not exist gets, which is what keeps the two
/// indistinguishable to a prober. A transport fault is [`FollowOpError::Failed`]:
/// not an answer about the address.
pub async fn resolve_owner_address<R>(nest: R, owner: &str) -> Result<ResolvedOwner, FollowOpError>
where
    R: RpcRequester,
    R::Error: RpcErrorClass,
{
    let owner = owner.trim();
    if let Ok(id) = ActorId::from_hex(owner) {
        return Ok(ResolvedOwner {
            actor_id: id.to_hex(),
            home_nest_url: String::new(),
        });
    }
    let Some((localpart, typed_domain)) = parse_fauna_handle(owner) else {
        return Err(FollowOpError::NotFound);
    };

    // Same-nest probe first — it also yields this nest's handle domain, which
    // disambiguates a typed `localpart@domain`. A transport fault is the answer
    // for a bare handle (the home nest is the only nest that could resolve it);
    // with a typed domain the peer may well be up while our own answer is
    // missing, so the shared rule's "home domain unknown → foreign" arm gives
    // the typed domain its own probe (`is_foreign_handle_domain`'s third case).
    let same = match ConversationsClient::new(nest)
        .actor_by_handle(localpart.clone())
        .await
    {
        Ok(same) => same,
        Err(e) if typed_domain.is_some() => {
            tracing::debug!(error = %e, "same-nest by_handle faulted; probing the typed domain");
            None
        }
        Err(e) => return Err(FollowOpError::Failed(e.to_string())),
    };
    // The verdict comes from the one owner of this decision rather than an
    // inline match: the conversations picker and the contacts knock send make
    // the identical call, and a per-surface copy is how `bob@other.test` comes
    // to mean two different actors on two pages of one app (priorities
    // #1/#3/#4). `same`'s echoed `domain` IS this nest's handle domain; a
    // `None` reply volunteers none, which the rule reads as "probe the typed
    // domain".
    let foreign = is_foreign_handle_domain(
        typed_domain.as_deref(),
        same.as_ref().map(|r| r.domain.as_str()),
    );
    if foreign {
        let domain = typed_domain
            .as_deref()
            .expect("foreign resolution implies a typed domain");
        return resolve_foreign_owner(&localpart, domain).await;
    }
    same.map(|reply| ResolvedOwner {
        actor_id: reply.actor_id.to_ascii_lowercase(),
        home_nest_url: String::new(),
    })
    .ok_or(FollowOpError::NotFound)
}

/// The cross-nest arm: the anonymous discovery hop against the peer, and the
/// peer's base URL as the record's home.
///
/// The reply's `addressable` (≥1 usable MLS key package) is deliberately not
/// consulted — it answers "can I message them", and a public folder's owner
/// need not be messageable to be followed.
async fn resolve_foreign_owner(
    localpart: &str,
    domain: &str,
) -> Result<ResolvedOwner, FollowOpError> {
    match fauna_client_conversations::actor_by_handle_remote(domain, localpart).await {
        Ok(Some(resolved)) => {
            let actor_id = ActorId::from_hex(&resolved.actor_id_hex)
                .map_err(|_| {
                    FollowOpError::Failed("foreign nest returned a malformed actor id".to_string())
                })?
                .to_hex();
            let home_nest_url = fauna_provisioning::probe::peer_nest_url(Some(domain.to_string()))
                .expect("a typed domain always derives a peer base url");
            Ok(ResolvedOwner {
                actor_id,
                home_nest_url,
            })
        }
        // A nest at that domain answered, and the answer is "no": unknown
        // handle, or a definite refusal from the disowning set (`domain_not_local`,
        // `handle.invalid`) — folded into the one not-found arm.
        Ok(None) | Err(ConvRpcError::Rejected { .. }) => Err(FollowOpError::NotFound),
        // No usable answer (DNS / connect / TLS / timeout), or a nest we cannot
        // talk to: a transport fault, never an answer about the address.
        Err(other) => Err(FollowOpError::Failed(other.to_string())),
    }
}

/// The handle a follow remembers for the owner address the user typed — the
/// followed row's owner label (`ui/folders.md` § Following a public folder:
/// *name + owner handle + badge + status*).
///
/// Taken at follow time because that is the one moment it is known: no kind maps
/// an actor id back to a handle, and the public plane names no owner in its
/// reply. A bare actor id typed in has no handle to remember (`None`). The reader
/// re-verifies what is remembered before showing it
/// (`public_follow::owner_handle_verdict`), since a handle can move on; a
/// remembered `localpart@domain` is re-checked with its domain
/// (`public_follow::verify_owner_handle`).
fn remembered_owner_handle(owner: &str) -> Option<String> {
    let owner = owner.trim();
    (!owner.is_empty() && ActorId::from_hex(owner).is_err()).then(|| owner.to_string())
}

/// **Follow a public folder** by the address the flow collects: the owner (handle
/// or 64-hex actor id) plus the folder's plaintext name.
///
/// Resolves the owner — and, from the handle's domain, the nest the folder is
/// homed on — runs the first public fetch (which is what pins the home nest's
/// stable `folder_id`, so a later rename cannot break the follow; the
/// follower's own nest relays it for a cross-nest home), and persists the
/// record through `store` — the account's `fauna.state.follows` row for the
/// folder. Returns the stored record; the caller re-runs its machine
/// `refresh()` so the row repaints with its availability.
///
/// Refuses a blank owner or name locally rather than sending an empty address:
/// an empty query would come back "not found" and read to the user as "no such
/// folder" rather than "you left a box empty".
pub async fn follow_public_folder<R, S>(
    nest: R,
    store: &S,
    owner: &str,
    folder_name: &str,
) -> Result<FollowedFolder, FollowOpError>
where
    R: RpcRequester + Clone,
    R::Error: RpcErrorClass,
    S: fauna_client_config::FollowsStore + ?Sized,
{
    let folder_name = folder_name.trim();
    let owner = owner.trim();
    if owner.is_empty() || folder_name.is_empty() {
        return Err(FollowOpError::NotFound);
    }
    let resolved = resolve_owner_address(nest.clone(), owner).await?;
    let owner_handle = remembered_owner_handle(owner);

    let page = crate::public_follow::resolve_public_folder(
        &nest,
        &resolved.home_nest_url,
        &resolved.actor_id,
        folder_name,
        0,
    )
    .await
    // The plane's own refusal is the "no such public folder" answer; anything
    // else is a transport fault and says so.
    .map_err(|e| match e {
        crate::public_follow::FollowError::Unavailable => FollowOpError::NotFound,
        other => FollowOpError::Failed(other.to_string()),
    })?;

    let record = FollowedFolder {
        owner_handle,
        ..page.record
    };
    fauna_client_config::save_follow(store, record.clone())
        .await
        .map_err(|e| FollowOpError::Failed(e.to_string()))?;
    Ok(record)
}

/// **Unfollow** — tombstone the account's row for the folder. Nothing is
/// revoked anywhere, because the home nest never knew this follower existed
/// (the public read plane keeps zero follower state by design). Idempotent.
pub async fn unfollow_public_folder<S>(
    store: &S,
    home_nest_url: &str,
    folder_id: i64,
) -> Result<(), FollowOpError>
where
    S: fauna_client_config::FollowsStore + ?Sized,
{
    fauna_client_config::save_unfollow(store, home_nest_url, folder_id)
        .await
        .map(|_| ())
        .map_err(|e| FollowOpError::Failed(e.to_string()))
}

#[cfg(test)]
mod follow_error_text_tests {
    use super::*;

    #[test]
    fn not_found_uses_the_single_ratified_wording() {
        assert_eq!(
            follow_error_text(FollowOpError::NotFound),
            fauna_i18n::strings::devices::ERROR_FOLLOW_NOT_FOUND
        );
    }

    /// A handle is remembered (trimmed) for the row's owner label; an actor id is
    /// not a handle, and there is nothing to remember from a blank box.
    #[test]
    fn a_follow_remembers_the_handle_it_was_addressed_by_and_never_an_actor_id() {
        assert_eq!(
            remembered_owner_handle("  alice ").as_deref(),
            Some("alice")
        );
        assert_eq!(
            remembered_owner_handle("bob@peer.example").as_deref(),
            Some("bob@peer.example")
        );
        assert_eq!(remembered_owner_handle(&"ab".repeat(32)), None);
        assert_eq!(remembered_owner_handle("   "), None);
    }

    #[test]
    fn failed_carries_the_underlying_message() {
        assert_eq!(
            follow_error_text(FollowOpError::Failed("timeout".to_string())),
            fauna_i18n::strings::devices::error_follow_failed("timeout")
        );
    }
}

/// The same-nest arms of the address chain against a canned nest. The foreign
/// arm dials a real peer (the anon hop) and is proven end to end by the tui
/// cross-nest follow e2e; its *decision* is `fauna_core::resolve`'s own,
/// pinned there.
#[cfg(test)]
mod resolve_owner_tests {
    use super::*;
    use fauna_client_testkit::{RejectingRequester, block_on};
    use fauna_protocol::discovery::ActorByHandleReply;

    const BY_HANDLE: &str = "fauna.actor.by_handle";

    fn nest_answering(handle: &str) -> RejectingRequester {
        RejectingRequester::new().reply(
            BY_HANDLE,
            &ActorByHandleReply {
                actor_id: "AB".repeat(32),
                handle: handle.to_string(),
                domain: "nest.example".to_string(),
                addresses: vec![],
                addressable: true,
                extra: Default::default(),
            },
        )
    }

    /// A pasted actor id is classified locally: no nest is asked, and the
    /// follow is homed on the caller's own nest.
    #[test]
    fn an_actor_id_asks_no_nest_and_is_homed_here() {
        let nest = RejectingRequester::new();
        let got = block_on(resolve_owner_address(&nest, &"AB".repeat(32))).expect("resolves");
        assert_eq!(got.actor_id, "ab".repeat(32));
        assert_eq!(got.home_nest_url, "");
        assert!(nest.kinds().is_empty());
    }

    /// A bare handle is by definition addressed on the nest the client is
    /// logged into: one same-nest probe, an empty home.
    #[test]
    fn a_bare_handle_resolves_on_the_callers_own_nest() {
        let nest = nest_answering("alice");
        let got = block_on(resolve_owner_address(&nest, " alice ")).expect("resolves");
        assert_eq!(got.actor_id, "ab".repeat(32), "normalized to lowercase");
        assert_eq!(got.home_nest_url, "");
        assert_eq!(nest.kinds(), [BY_HANDLE]);
    }

    /// A typed domain that is this nest's own (case-insensitively, as DNS
    /// labels are) is the same-nest follow — the picker's exact rule, so
    /// `alice@nest.example` and `alice` name one actor on one nest.
    #[test]
    fn a_handle_at_the_callers_own_domain_is_the_same_nest_follow() {
        let nest = nest_answering("alice");
        let got = block_on(resolve_owner_address(&nest, "alice@NEST.example")).expect("resolves");
        assert_eq!(got.home_nest_url, "");
        assert_eq!(
            nest.kinds(),
            [BY_HANDLE],
            "the localpart alone is probed here"
        );
    }

    /// An unknown bare handle folds into the one not-found arm — the same
    /// answer a folder that does not exist gets.
    #[test]
    fn an_unknown_bare_handle_is_the_folded_not_found() {
        let nest = RejectingRequester::new().reject(
            BY_HANDLE,
            fauna_protocol::RpcError::new("fauna.actor.not_found", "error.actor.not_found"),
        );
        let err = block_on(resolve_owner_address(&nest, "nobody")).expect_err("not found");
        assert!(matches!(err, FollowOpError::NotFound), "got {err:?}");
    }

    /// Shapes that belong to other rails are not Fauna handles and fold into
    /// not-found without a probe.
    #[test]
    fn a_non_fauna_shape_is_not_found_without_asking() {
        let nest = RejectingRequester::new();
        for junk in ["@user@instance", "did:plc:abc", "npub1abc", "two words"] {
            let err = block_on(resolve_owner_address(&nest, junk)).expect_err(junk);
            assert!(matches!(err, FollowOpError::NotFound), "{junk}: {err:?}");
        }
        assert!(nest.kinds().is_empty());
    }

    /// A transport fault on the caller's own nest, for a bare handle, is a
    /// fault — never the folded not-found (a network problem must never read as
    /// "no such folder").
    #[test]
    fn a_faulting_home_nest_fails_a_bare_handle_rather_than_folding_it() {
        // An unmapped kind is the double's transport fault.
        let nest = RejectingRequester::new();
        let err = block_on(resolve_owner_address(&nest, "alice")).expect_err("fault");
        assert!(matches!(err, FollowOpError::Failed(_)), "got {err:?}");
    }
}
