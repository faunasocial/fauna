//! **The nest custody door's authorization — the one predicate every door
//! shares.**
//!
//! Owner: `docs/goal/architecture/account-data-plane.md` § The custody grant +
//! ceremony → *Coverage enumeration* (the ruling) and § Implementation status
//! today → *Built — W8.6 (account-data-plane.md § Workstreams)* (the door).
//!
//! `CallerClass::Custodian` now reaches four doors, in two planes:
//!
//! | Plane | Door | Yields |
//! |---|---|---|
//! | index | `fauna.sync.changes.list` (class-2 arm) | the owner's sealed state entries |
//! | index | `fauna.sync.changes.list` (class-1 arm) | record-CID *coordinates* |
//! | bulk  | `fauna.segments.list` | which segments hold the bytes those coordinates name |
//! | bulk  | `GET /api/v1/segments/{kind}/{actor}/{id}[/meta]` | the bytes themselves |
//!
//! They were built in that order, one slice each, and the reason this module
//! exists is that **the fourth door is only as strong as the weakest of the
//! four**: a coordinate a custodian may see but not fetch is merely useless,
//! while bytes it may fetch without being allowed to see the coordinate is a
//! disclosure. Slice 1 already learned the smaller version of this lesson —
//! the two feed arms were made to share [`custody_admits_scope`] "so the two
//! arms cannot drift" — and slice 2 doubles the number of callers, which is
//! what moved the predicate out of `sync_handlers` and into a module of its
//! own. **Add a door, call [`admit_content_scope_for_custody`] (or its
//! segment-plane spelling [`admit_segment_plane_for_custody`]) — never
//! re-derive the verdict.**

use fauna_protocol::RpcError;

use crate::routes::AppState;

/// **Does `holder` hold a live custody row over `owner` that admits `scope`?**
///
/// Authorization is the LIVE capability row, re-derived RIGHT HERE on every
/// request (the "per-request row re-check"): `fauna.capabilities.revoke`
/// deletes the row and the very next request on a live session refuses. There
/// is no custody session registry to go stale — that statelessness is what
/// makes revocation sever a live session rather than merely bar the next one.
/// A caller with no live row for the named owner is refused, never silently
/// self-served.
///
/// The verdict is the row's own scope tuples read through `AdmittedScopes` —
/// the one vocabulary the peer seam evaluates too, so the two doors cannot
/// drift into two answers for one grant (`account-data-plane.md` § The custody
/// grant + ceremony).
///
/// ⚠ **`AdmittedScopes` answers only the SCOPE half — the caller must bind the
/// ACCOUNT half, and for content scopes that is not free.** The enum's own doc
/// says the verdict's other halves (account, validity bound) stay with their
/// evaluators. On the account-state arm that binding is automatic: the scope is
/// per-account and `owner` is what the row lookup uses. On any content door it
/// is **not**: a content scope string carries its own `scope_id`, so
/// `AllOfAccountSinglePrincipal.admits("content:post:<stranger>")` is `true` —
/// it shape-checks and is not co-authored. Content callers must therefore go
/// through [`admit_content_scope_for_custody`], never this function directly.
///
/// ⚠ **The window check is BOTH bounds** — `capability_grants` stores only
/// `epoch_end`, so the storage filter that hands this loop its rows is half a
/// window check wearing a whole one's name. Finding is what a decoded row
/// looks like when nobody asks the other half: a custodian holding one live
/// narrow row and one post-dated wide row minted on the live one and pulled
/// what only the post-dated one admitted. `grant_window_is_open` fails closed
/// and is called at every site that re-derives authorization from a decoded
/// grant. Adding a door here means calling it too — the census walk
/// `every_authorizing_grant_decode_checks_the_whole_window` is what notices
/// when a new site forgets.
pub(crate) async fn custody_admits_scope(
    state: &AppState,
    holder: &[u8; 32],
    owner: &[u8; 32],
    scope: &str,
) -> Result<(), RpcError> {
    use fauna_core::custody_grant::CustodyScopeSet;
    use fauna_protocol::scope::AdmittedScopes;

    let now = crate::db::now_epoch_secs();
    for blob in state
        .db
        .fetch_capability_grants_for_holder(holder, now)
        .await
        .map_err(|e| crate::rpc_errors::internal_ns("sync", format!("custody grant lookup: {e}")))?
    {
        let Ok(grant) = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&blob) else {
            continue; // an unreadable row authorizes nothing
        };
        if !fauna_mls::wrapped_blob::grant_window_is_open(&grant, now) {
            continue; // outside the grant's validity window; see the helper
        }
        if grant.index.0.as_slice() != owner.as_slice() {
            continue;
        }
        let Some(set) = fauna_mls::wrapped_blob::custody_scope_set_from_tuples(&grant.scope) else {
            continue; // not a custody row
        };
        let verdict = match set {
            CustodyScopeSet::Account => AdmittedScopes::AllOfAccountSinglePrincipal,
            CustodyScopeSet::Scopes(named) => AdmittedScopes::Named(named),
            // A set this build cannot name covers no scope.
            CustodyScopeSet::Unknown(_) => AdmittedScopes::Named(Vec::new()),
        };
        if verdict.admits(scope) {
            return Ok(());
        }
    }
    Err(crate::rpc_errors::permission_denied_ns(
        "sync",
        "no live custody grant admits this scope for the named owner",
    ))
}

/// **Custody admission for a CONTENT scope — the nest-door half of the
/// coverage-enumeration ruling** (`account-data-plane.md` § The custody grant +
/// ceremony → *Coverage enumeration*; row 148).
///
/// The ruling's nest-door corollary, in its own words: *"under an `Account` row
/// the door's content coverage is the same pure function of the row's owner; a
/// `conv` scope requested under an `Account` grant refuses loudly (the
/// record-cid arm's loud-refusal rule), never serves and never empty-answers."*
/// Three properties fall out, and each is load-bearing:
///
/// **(1) The scope id must BE the owner.** [`custody_admits_scope`] answers the
/// scope half; the account half is this function's job, and on content scopes
/// it is the whole security. `AllOfAccountSinglePrincipal` admits any
/// well-formed non-co-authored scope string — including a stranger's — so an
/// `Account` row over `owner` would otherwise serve `content:post:<stranger>`
/// to a custodian that named that owner. The check is the ruling's "pure
/// function of the row's owner": the covered content set is exactly
/// `derive_own_actor_scopes(owner)`.
///
/// **(2) The predicate is the verifier's, not the custodian's.** Coverage is
/// decided by [`fauna_protocol::scope::is_co_authored_scope`] — a deliberate
/// **blocklist**, so a future single-principal kind is covered with no re-mint
/// (the no-silent-decay constraint) — rather than by mirroring
/// `fauna_sync_engine::scope_set::OWN_ACTOR_KINDS`, which is an allowlist a
/// custodian binary may be behind on. The ruling settles the direction: *"the
/// verifier's predicate stays authoritative"*, and an older custodian that does
/// not know a newly-registered kind simply pulls less — honest under-pull, not
/// a refusal.
///
/// **(3) A `conv` scope under an `Account` row refuses LOUDLY — structurally.**
/// The coverage-enumeration ruling names this case specifically, because it is
/// the metadata boundary `principles.md` keeps sealed ("which conversations
/// does this account have"). No dedicated `Account`-vs-conv refusal arm exists
/// or is needed: on the co-authored branch [`custody_admits_co_authored_scope`]
/// matches only explicit-list rows (an `Account` row is skipped, never
/// widened), and on the historical single-principal path
/// `AllOfAccountSinglePrincipal.admits()` already excluded co-authored scopes
/// via `is_co_authored_scope`. Pinned by the tier_3 conv arm, which asserts
/// the refusal without caring which check produced it.
///
/// The explicit-list form is self-naming (the owner disclosed exactly those
/// scope ids at mint), so `AdmittedScopes::Named`'s exact string match answers
/// the scope half — and since the member-mint rule (2026-08-18), a co-authored
/// entry additionally binds its **grantor to the channel's current roster** at
/// every request ([`custody_admits_co_authored_scope`]).
pub(crate) async fn admit_content_scope_for_custody(
    state: &AppState,
    holder: &[u8; 32],
    owner: &[u8; 32],
    scope_text: &str,
    content: &fauna_protocol::scope::ContentScope,
) -> Result<(), RpcError> {
    // A co-authored scope (a conv channel) has its own account half — the
    // member-mint rule — evaluated inside its own row walk, with the caller's
    // named owner pinned as the required grantor.
    if fauna_protocol::scope::is_co_authored_scope(scope_text) {
        return custody_admits_co_authored_scope(
            state,
            holder,
            scope_text,
            content.scope_id(),
            Some(owner),
        )
        .await;
    }

    // The scope half + the live-row re-check, shared with the class-2 arm.
    custody_admits_scope(state, holder, owner, scope_text).await?;

    // The account half.
    if content.scope_id() != owner {
        return Err(crate::rpc_errors::permission_denied_ns(
            "sync",
            "a single-principal content scope is served only for the custodied \
             owner it belongs to",
        ));
    }
    Ok(())
}

/// **Custody admission for a CO-AUTHORED content scope — the member-mint rule**
/// ([`account-data-plane.md`] § The custody grant + ceremony → *Shared-audience
/// carve-out*, ruled 2026-08-18; serve-plane contract:
/// `message-segment-store.md` § *Which kinds the two planes serve*).
///
/// A co-authored scope's id is a channel, never an actor, so the
/// single-principal account binding cannot apply. What binds instead: the
/// authorizing row must be the **explicit-list form naming exactly this scope**
/// (an `Account` row never covers a co-authored scope — the carve-out, enforced
/// here structurally by matching only `CustodyScopeSet::Scopes` rows), and the
/// row's owner — the granting member — must be a **current member** of the
/// channel the scope names. Membership is re-derived on every request beside
/// the live-row re-check, so leaving the channel severs a live custody session
/// at its very next request, exactly as `fauna.capabilities.revoke` does.
///
/// `required_owner` distinguishes the two doors: the index plane's request
/// names the custodied owner (`of_owner`), so the row must be that owner's;
/// the bulk plane's request carries only the scope id, so any live qualifying
/// row of the holder's admits (`None`) — the grant self-identifies its owner,
/// and the membership check is what binds that owner to the channel.
///
/// The refusal is the same `permission_denied` in every direction — holder
/// with no row, an `Account`-only holder, a non-member grantor — so a refused
/// custodian learns exactly what a stranger learns (the door-oracle rule).
async fn custody_admits_co_authored_scope(
    state: &AppState,
    holder: &[u8; 32],
    scope: &str,
    channel_id: &[u8; 32],
    required_owner: Option<&[u8; 32]>,
) -> Result<(), RpcError> {
    use fauna_core::custody_grant::CustodyScopeSet;
    use fauna_protocol::scope::AdmittedScopes;

    let now = crate::db::now_epoch_secs();
    for blob in state
        .db
        .fetch_capability_grants_for_holder(holder, now)
        .await
        .map_err(|e| crate::rpc_errors::internal_ns("sync", format!("custody grant lookup: {e}")))?
    {
        let Ok(grant) = fauna_mls::wrapped_blob::GrantBlob::from_canonical_bytes(&blob) else {
            continue; // an unreadable row authorizes nothing
        };
        if !fauna_mls::wrapped_blob::grant_window_is_open(&grant, now) {
            continue; // outside its validity window; see custody_admits_scope
        }
        if let Some(owner) = required_owner
            && grant.index.0.as_slice() != owner.as_slice()
        {
            continue;
        }
        let Some(set) = fauna_mls::wrapped_blob::custody_scope_set_from_tuples(&grant.scope) else {
            continue; // not a custody row
        };
        // The carve-out, structurally: only the explicit-list form can name a
        // co-authored scope. An `Account` row is skipped — never widened.
        let CustodyScopeSet::Scopes(named) = set else {
            continue;
        };
        if !AdmittedScopes::Named(named).admits(scope) {
            continue;
        }
        // The member-mint rule's account half: the granting member must be a
        // CURRENT member of the room the scope names — the roster read is the
        // re-derivation.
        //
        // ⚠ The authority is the **floor roster** (`room_members`; the
        // channel id IS the room id — `conversation-rooms.md` § The room),
        // NEVER `actor_channels` — finding : `actor_channels` is the
        // channel's ROUTING roster, and a single `channel.send` auto-registers
        // its sender there on exactly this plane's population, so a row in it
        // proves knowledge of the channel id, not membership.
        //
        // The floor roster is the non-self-assertable membership record this
        // door was declared to wait for
        // (`conversation-rooms.md` § The floor roster — "the custody serve
        // door reads the floor roster, for both classes";
        // `account-replica-posture.md` § Shared-audience carve-out adopts it
        // by reference). It replaced the `group_members` read on 2026-09-09,
        // the first code step of § The group plane's fate — before any group
        // kind or table is retired, so no serving path ever points at a
        // retired table.
        //
        // Its authority differs by class, and the door does not care which:
        // for a community room the roster is authoritative, for an end-to-end
        // room it is a member-reported mirror
        // (`fauna.conversations.room.roster_report`). That is sound *here*
        // precisely because this door decides serving, not reading — a wrong
        // report cannot make a non-member open a sealed envelope, and what a
        // custodian gains is bytes it already holds. It is the same
        // report-never-guess record the succession sweep uses.
        //
        // A room with no floor-roster rows still fails CLOSED — honest
        // under-coverage, now ended for every room whose members' apps report
        // (rather than for none at all, which is where the `group_members`
        // read left it: no shipped app ever wrote a row to that table).
        let Ok(grantor) = <[u8; 32]>::try_from(grant.index.0.as_slice()) else {
            continue; // a malformed owner id authorizes nothing
        };
        // Membership, not rank: a policy-less room's members carry no role at all,
        // and a door keyed on the role would fail closed for exactly the
        // rooms that exist today.
        let is_member = state
            .db
            .is_room_member(channel_id, &grantor)
            .await
            .map_err(|e| {
                crate::rpc_errors::internal_ns("sync", format!("floor roster lookup: {e}"))
            })?;
        if is_member {
            return Ok(());
        }
    }
    Err(crate::rpc_errors::permission_denied_ns(
        "sync",
        "no live custody grant admits this scope for the named owner",
    ))
}

/// **Custody admission for the BULK plane** — `fauna.segments.list` and the
/// `GET /api/v1/segments/{kind}/{actor_hex}/{segment_id}[/meta]` pair route.
///
/// The bulk plane addresses bytes by `(kind, actor)`, where the index plane
/// addresses coordinates by a scope string plus a separate `of_owner`. Those
/// are the same authorization question asked in two vocabularies, so this
/// function is a pure translation into [`admit_content_scope_for_custody`]:
/// `(kind, actor)` becomes the content scope `content:<kind>:<actor_hex>`, and
/// `actor` is *both* the custodied owner and the scope id.
///
/// **That collapse is a security property, not a shortcut.** On the feed a
/// caller supplies `scope` and `of_owner` independently, so the two must be
/// checked against each other (the account binding of
/// [`admit_content_scope_for_custody`] property (1)). Here there is one actor
/// field and it plays both roles, so `scope_id == owner` holds by construction
/// and a request for a third party's bytes is not merely refused but
/// **unrepresentable**: naming a stranger's actor asks for a grant over the
/// *stranger*, which this custodian does not hold. Both spellings still run
/// through the same predicate, because "unrepresentable" is a property of
/// today's request shape and the predicate is what survives a wire change.
///
/// Returns the refusal for the caller to map onto its own surface's error
/// vocabulary — `fauna.segments.not_owner` on the control plane, `403` on the
/// byte route — so a custodian's refusal is **indistinguishable from an
/// unrelated stranger's**, which is what keeps the door from answering "this
/// grant exists but does not cover that".
pub(crate) async fn admit_segment_plane_for_custody(
    state: &AppState,
    holder: &[u8; 32],
    owner: &[u8; 32],
    kind: &str,
) -> Result<(), RpcError> {
    // A placement journal is served on the segment plane, but it is NOT a
    // content plane: it rests as floor plaintext (mailbox names, flags), where
    // every content kind rests sealed. So no grant reaches it, whatever the
    // grant says — and that cannot be left to the grant's own vocabulary,
    // because a journal tag is a well-formed kind tag and an all-of-account
    // custody row admits every well-formed content scope it is asked about.
    // Refused before any row is read, with the same answer a stranger gets
    // (`segment-backup-protocol.md` § *Which kinds the two planes serve*).
    if fauna_sync_engine::segment_backup::is_placement_serve_kind(kind) {
        return Err(crate::rpc_errors::permission_denied_ns(
            "sync",
            "a placement journal is served to its owner alone",
        ));
    }
    let content = fauna_protocol::scope::ContentScope::new(kind, *owner).map_err(|_| {
        // A kind the scope vocabulary does not know cannot name a covered
        // plane, so this is a refusal rather than a malformed-request error:
        // the caller learns "not yours", the same answer a stranger gets.
        crate::rpc_errors::permission_denied_ns("sync", "not a content-scope kind")
    })?;
    let scope_text = content.to_string();

    // A co-authored kind breaks the collapse: the request's one actor field is
    // the SCOPE ID (a conv channel), never an owner — no grantor is named on
    // the wire, the authorizing row self-identifies its owner, and the
    // member-mint rule is what binds that owner to the channel
    // (`message-segment-store.md` § *Which kinds the two planes serve*).
    if fauna_protocol::scope::is_co_authored_scope(&scope_text) {
        return custody_admits_co_authored_scope(
            state,
            holder,
            &scope_text,
            content.scope_id(),
            None,
        )
        .await;
    }

    admit_content_scope_for_custody(state, holder, owner, &scope_text, &content).await
}
