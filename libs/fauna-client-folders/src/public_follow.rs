//! The **publicly-synced follow**'s read op — `fauna.folders.public.fetch`
//! from the follower's side (`docs/goal/behavior/folders.md` § Publicly-synced
//! follow; the kinds and gates: `docs/goal/architecture/federation.md` § The
//! public folder read plane). Folders re-model phase 4 slice 4f-ii.
//!
//! The follower's read mechanisms live here — and only the pure/portable ones,
//! so every app and both transport arms share one behavior:
//!
//! * [`resolve_public_folder`] — the **first contact**, addressed by
//!   `(owner, plaintext name)`. Its whole job is to turn that address into the
//!   [`FollowedFolder`] record the follow *is*, pinning the home nest's stable
//!   `folder_id` so a later rename cannot break the follow.
//! * [`fetch_followed_changes`] — every read after that, addressed by the
//!   pinned id.
//! * [`availability_from_probe`] / [`needs_probe`] / [`resolve_availability`] —
//!   the availability verdict + its staleness-budgeted bounded fan-out.
//! * [`listing_from_changes`] — the **head fold** turning a page of raw public
//!   change rows into the browsable file listing (the Media followed browse
//!   scope, `docs/goal/ui/media.md` § Followed public folders).
//! * [`download_followed_file`] — the keyless byte read.
//!
//! **Failure is deliberately uninformative, and callers must keep it that way.**
//! Absent, private, un-declassified and misspelled all answer one
//! indistinguishable `not_found` — the nest folds them on purpose (ST-RES-1), so
//! a caller that tried to tell them apart for a friendlier message would be
//! building the existence oracle the fold exists to prevent. [`FollowError`]
//! therefore carries exactly one domain arm, and `ui/folders.md` § Following a
//! public folder specifies the single user-facing wording for it.
//!
//! **Nothing here writes follow state to any nest.** The home nest keeps none,
//! and the follower's own record is persisted by
//! `fauna_client_config::save_follow` — this module only reads.

use fauna_core::data::FollowedFolder;
use fauna_protocol::folders::{
    FoldersPublicFetchReply, FoldersPublicFetchRequest, KIND_FOLDERS_PUBLIC_FETCH,
};
use fauna_protocol::sync::SyncChange;
use fauna_protocol::{RpcErrorClass, RpcRequester};

/// Why a public-folder read could not complete.
#[derive(Debug)]
pub enum FollowError<E> {
    /// Transport / RPC failure — the caller's own error type.
    Rpc(E),
    /// **No public folder at that address.** Absent, private, group-bound but
    /// never declassified, deleted, or simply misspelled — the home nest
    /// answers all of them identically by design, and this arm preserves that.
    /// It is also what a flip-back looks like: an established follow whose owner
    /// re-sealed the folder starts answering this, which is the revoke.
    Unavailable,
}

impl<E: std::fmt::Display> std::fmt::Display for FollowError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc(e) => write!(f, "{e}"),
            Self::Unavailable => write!(f, "no public folder at that address"),
        }
    }
}

/// One page of a followed folder's public change log, plus the folder meta the
/// reply re-stamps on every read.
#[derive(Debug, Clone, PartialEq)]
pub struct PublicFolderPage {
    /// The record as the home nest currently describes it — the caller persists
    /// this on a first follow, and may refresh the stored copy from it later
    /// (the display name tracks the folder's current name).
    pub record: FollowedFolder,
    /// Floor-filtered, stripped change rows. `device_id`, `author_actor_id`,
    /// `path_sealed` and `content_key_version` carry no value here — a follower
    /// gets paths, manifest refs, sizes and timestamps, never the owner's device
    /// fleet or authorship map.
    pub changes: Vec<SyncChange>,
}

/// The nest's one refusal on this plane (`folder_public::refuse`). Every
/// address that does not name a currently-public folder answers exactly this.
const NOT_FOUND: &str = "fauna.folders.not_found";

/// Map an RPC error onto [`FollowError`], folding the nest's single refusal
/// into the one uninformative arm.
///
/// Matched on the wire **code**, never on the detail string: the code is the
/// stable identifier, and the detail is deliberately identical across absent /
/// private / reserved so that nothing downstream can reconstruct which one it
/// was. A transport fault stays [`FollowError::Rpc`] — "the network is down" and
/// "this folder is not public" must never render the same, or a follower cannot
/// tell a broken connection from a revoked follow.
fn classify<E: RpcErrorClass>(err: E) -> FollowError<E> {
    match err.as_rpc_error() {
        Some(e) if e.code == NOT_FOUND => FollowError::Unavailable,
        _ => FollowError::Rpc(err),
    }
}

/// Build the [`FollowedFolder`] a reply describes, at `home_nest_url`.
///
/// `owner_handle` is carried through from the caller, never read off the reply:
/// the public plane names no owner (a stranger enumerating folder ids must not
/// learn whose each one is), so the handle is only ever what the follower typed.
fn record_from(
    reply: &FoldersPublicFetchReply,
    home_nest_url: &str,
    owner_actor_id: &str,
    owner_handle: Option<String>,
) -> FollowedFolder {
    FollowedFolder {
        home_nest_url: home_nest_url.to_string(),
        home_nest_actor_id: reply.home_nest_actor_id.clone(),
        owner_actor_id: owner_actor_id.to_string(),
        owner_handle,
        folder_id: reply.folder_id,
        display_name: reply.name.clone(),
    }
}

/// **First contact**: resolve `(owner, plaintext name)` and pin the result.
///
/// `home_nest_url` empty ⇒ the folder is homed on the caller's own nest (the
/// same-nest follow); otherwise the caller's nest relays. The returned page's
/// `record` is what the caller persists via `fauna_client_config::save_follow`.
pub async fn resolve_public_folder<R: RpcRequester>(
    nest: &R,
    home_nest_url: &str,
    owner_actor_id: &str,
    folder_name: &str,
    since: i64,
) -> Result<PublicFolderPage, FollowError<R::Error>>
where
    R::Error: RpcErrorClass,
{
    let reply: FoldersPublicFetchReply = nest
        .request(
            KIND_FOLDERS_PUBLIC_FETCH,
            FoldersPublicFetchRequest {
                nest_url: (!home_nest_url.is_empty()).then(|| home_nest_url.to_string()),
                owner_actor_id: Some(owner_actor_id.to_string()),
                folder_name: Some(folder_name.to_string()),
                folder_id: None,
                since,
                limit: 0,
                extra: Default::default(),
            },
        )
        .await
        .map_err(classify)?;
    Ok(PublicFolderPage {
        record: record_from(&reply, home_nest_url, owner_actor_id, None),
        changes: reply.changes,
    })
}

/// Every read after the first: address by the **pinned** `folder_id`, so a
/// rename on the home nest cannot break an established follow.
///
/// [`FollowError::Unavailable`] here is the flip-back — the owner re-sealed the
/// folder, or deleted it. `ui/folders.md` § Following a public folder specifies
/// what the row shows then (a loud *no longer available* state that stays until
/// the user removes it; a re-flip resumes the follow under the same id).
pub async fn fetch_followed_changes<R: RpcRequester>(
    nest: &R,
    followed: &FollowedFolder,
    since: i64,
) -> Result<PublicFolderPage, FollowError<R::Error>>
where
    R::Error: RpcErrorClass,
{
    let reply: FoldersPublicFetchReply = nest
        .request(
            KIND_FOLDERS_PUBLIC_FETCH,
            FoldersPublicFetchRequest {
                nest_url: (!followed.home_nest_url.is_empty())
                    .then(|| followed.home_nest_url.clone()),
                owner_actor_id: None,
                folder_name: None,
                folder_id: Some(followed.folder_id),
                since,
                limit: 0,
                extra: Default::default(),
            },
        )
        .await
        .map_err(classify)?;
    Ok(PublicFolderPage {
        record: record_from(
            &reply,
            &followed.home_nest_url,
            &followed.owner_actor_id,
            followed.owner_handle.clone(),
        ),
        changes: reply.changes,
    })
}

/// Fold a probe result into the pair a followed row renders: **(still served,
/// name to show)**.
///
/// Split out as a pure function on purpose. The rule it encodes is
/// security-and-UX load-bearing, but its only production caller is transport
/// glue built over a concrete `NestClient` that no unit test can stand up — so
/// leaving the rule inside that glue would have meant shipping it untested. The
/// mechanism is testable; only the wiring is not.
///
/// The rule: **only the plane's own refusal means "no longer available".**
///
/// * `Ok` — served. The name is refreshed from the reply, so a rename on the
///   home nest shows up as a new label rather than a stale one.
/// * `Err(Unavailable)` — the revoke. The owner flipped the audience back or
///   deleted the folder; the row keeps its last known name so it stays
///   recognisable while the user decides whether to remove it.
/// * `Err(Rpc(_))` — a transport fault, which is **not** an answer about the
///   folder. The row stays available with its stored name, because showing the
///   revoke state on a dropped connection would tell the user their follow was
///   revoked every time their network blipped.
pub fn availability_from_probe<E>(
    probe: Result<PublicFolderPage, FollowError<E>>,
    stored: &FollowedFolder,
) -> (bool, String) {
    match probe {
        Ok(page) => (true, page.record.display_name),
        Err(FollowError::Unavailable) => (false, stored.display_name.clone()),
        Err(FollowError::Rpc(_)) => (true, stored.display_name.clone()),
    }
}

/// How long an availability verdict is reused before a fresh probe is worth
/// spending a cross-nest round trip on.
///
/// **Sized against what the verdict actually is, not against how often the page
/// refreshes.** Availability changes when an owner flips a folder's audience
/// back or deletes it — a once-ever event per follow, not a live signal. The
/// devices page refreshes on every nav edge and on pushes, so probing per
/// refresh spends N relayed round trips to re-learn a value that almost never
/// moves.
///
/// One minute is deliberately modest rather than aggressive: the cost of
/// staleness is a row that reads `Following` for up to a minute after a revoke
/// (or `no longer available` for up to a minute after a re-flip), and the
/// ratified UX already tolerates exactly that in the other direction — a
/// transport fault keeps a row available indefinitely, by design, because a
/// dropped connection must not read as a revoke
/// ([`availability_from_probe`]). A TTL therefore introduces no *kind* of
/// staleness the contract did not already accept.
pub const AVAILABILITY_TTL_MS: u64 = 60_000;

/// How many availability probes may be in flight at once.
///
/// **Bounded, not `join_all`.** Each probe is relayed by the follower's OWN
/// nest out to a home nest, so N concurrent probes become N concurrent relays
/// into the per-source-IP and per-nest throttles the public read plane rides by
/// design (`federation.md` § The public folder read plane, *Zero state, zero
/// metering*). Serial was safe from that and unusably slow; an unbounded fan-out
/// is fast and trips the very throttles that keep the plane cheap to serve.
/// Four keeps a cold first render to `ceil(N/4)` round trips while staying far
/// under any throttle a handful of follows could reach.
pub const AVAILABILITY_PROBE_CONCURRENCY: usize = 4;

/// One remembered availability verdict, with the wall-clock it was taken at.
///
/// Carries the resolved `display_name` alongside the flag because the two come
/// from the same probe and must not drift apart: a serving folder's name is the
/// one the home nest just returned, a revoked one keeps the stored name so the
/// row stays recognisable while the user decides whether to remove it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedAvailability {
    pub available: bool,
    pub display_name: String,
    /// The owner handle the row may show — the record's remembered
    /// [`FollowedFolder::owner_handle`] **as last verified** against its actor id
    /// ([`owner_handle_verdict`]), or `None` when there is none or it no longer
    /// names that actor. Rides this verdict because it is checked beside the
    /// same probe, under the same staleness budget.
    pub owner_handle: Option<String>,
    /// `fauna_core::data::Timestamp::now_millis()` at the time of the probe.
    pub probed_at_ms: u64,
}

/// Whether a cached verdict is too old to reuse at `now_ms`.
///
/// Never-probed (`None`) always needs one.
///
/// ⚠ A **future-stamped** entry re-probes rather than reading as fresh. A clock
/// that steps backwards (a laptop waking, an NTP correction) leaves stamps ahead
/// of `now_ms`, and the obvious `now_ms.saturating_sub(probed_at)` saturates to
/// `0` there — i.e. "probed this instant" — which freezes the row at whatever it
/// last said for as long as the skew lasts. That is the precise bug this
/// ordering avoids, and it is why the comparison is written as two clauses
/// rather than one subtraction. (Caught by its own test, which failed against
/// the saturating form.)
pub fn needs_probe(cached: Option<&CachedAvailability>, now_ms: u64, ttl_ms: u64) -> bool {
    match cached {
        None => true,
        Some(entry) => entry.probed_at_ms > now_ms || now_ms - entry.probed_at_ms >= ttl_ms,
    }
}

/// Whether `handle` still names `owner_actor_id` on the caller's own nest —
/// one `fauna.actor.by_handle` read.
///
/// `Ok(false)` when the handle no longer resolves at all (the rejections
/// [`fauna_protocol::discovery::is_unresolved_handle_code`] names) or resolves
/// to someone else — a handle can be changed and later taken; `Err` only for a
/// fault, which is not an answer about the handle. Actor ids compare
/// case-insensitively: hex arrives in either case across callers.
///
/// A remembered `localpart@domain` (the follow was addressed with a domain —
/// `follow_ops` accepts the picker's superset) is checked by its **localpart**,
/// and still names the owner only while the nest's echoed domain is the typed
/// one: the same [`fauna_core::resolve::is_foreign_handle_domain`] verdict the
/// follow itself was routed by, so `alice@nest.example` verifies exactly when
/// `alice` does and a domain this nest does not serve never verifies. A shape
/// that is not a Fauna handle at all verifies false without a read.
pub async fn verify_owner_handle<R: RpcRequester>(
    nest: &R,
    handle: &str,
    owner_actor_id: &str,
) -> Result<bool, R::Error>
where
    R::Error: RpcErrorClass,
{
    use fauna_protocol::discovery::{
        ActorByHandleReply, ActorByHandleRequest, is_unresolved_handle_code,
    };
    let Some((localpart, typed_domain)) = fauna_core::resolve::parse_fauna_handle(handle) else {
        return Ok(false);
    };
    match nest
        .request::<_, ActorByHandleReply>(
            "fauna.actor.by_handle",
            ActorByHandleRequest {
                handle: localpart,
                domain: None,
                extra: Default::default(),
            },
        )
        .await
    {
        Ok(reply) => Ok(reply.actor_id.eq_ignore_ascii_case(owner_actor_id)
            && !fauna_core::resolve::is_foreign_handle_domain(
                typed_domain.as_deref(),
                Some(reply.domain.as_str()),
            )),
        Err(e)
            if e.as_rpc_error()
                .is_some_and(|rpc| is_unresolved_handle_code(&rpc.code)) =>
        {
            Ok(false)
        }
        Err(e) => Err(e),
    }
}

/// Fold one owner-handle check into the handle a followed row may show.
///
/// Pure for the reason [`availability_from_probe`] is: its only production
/// caller is transport glue no unit test can stand up.
///
/// * no remembered handle → `None` (the row shows the actor id's short form);
/// * `verified == Some(Ok(true))` → the handle, which still names the owner;
/// * `Some(Ok(false))` → `None`: the handle now resolves to nobody or to someone
///   ELSE, and painting it would say the folder is that person's;
/// * a fault (`Some(Err(_))`), or no check at all (`None`), is not an answer:
///   the last verdict stands, and a never-checked record keeps the handle it
///   was followed by — it named the owner when the follow resolved it, the
///   same "last known value" rule a transport fault gets for availability.
///   `None` is every read that is not an owner check (a browse fetch) and every
///   follow homed on another nest, which is deliberately never re-checked
///   ([`owner_handle_checkable`]): the follow itself verified that handle
///   against the owner's nest, and that verdict stands until the user
///   unfollows (ratified 2026-09-22, `ui/folders.md` § Following a public
///   folder).
pub fn owner_handle_verdict<E>(
    remembered: Option<&str>,
    verified: Option<Result<bool, E>>,
    previous: Option<&CachedAvailability>,
) -> Option<String> {
    let handle = remembered.filter(|h| !h.is_empty())?;
    match verified {
        Some(Ok(true)) => Some(handle.to_string()),
        Some(Ok(false)) => None,
        Some(Err(_)) | None => match previous {
            Some(prev) => prev.owner_handle.clone(),
            None => Some(handle.to_string()),
        },
    }
}

/// Whether the probe pass spends a `fauna.actor.by_handle` read re-checking a
/// follow's remembered owner handle: only for a follow homed on the caller's
/// own nest, whose handles that read answers for.
///
/// A follow homed on another nest is **deliberately not re-checked**, although
/// the follow gesture resolved its `handle@domain` through the anonymous hop to
/// that peer and could again (`follow_ops::resolve_owner_address`): the
/// availability probe is *relayed* by the follower's own nest, so the follower's
/// client never dials the owner's nest on a timer today — re-checking would
/// add a recurring anonymous connection from the follower's own address to the
/// owner's nest with no user action behind it, against a kind the peer
/// rate-limits for anonymous callers. The handle the follow was resolved by
/// stands instead ([`owner_handle_verdict`]'s no-check arm) — it was verified
/// against the owner's nest at follow time, and the follow is pinned to the
/// actor id regardless, so a handle that later moves on the peer mislabels the
/// row at worst, never re-points it.
fn owner_handle_checkable(record: &FollowedFolder) -> bool {
    record.home_nest_url.is_empty()
}

/// The verdict a **browse fetch** writes into the cache the probe path keeps
/// (`ui/media.md` § Followed public folders: *a browse fetch is availability
/// evidence*): availability and name from the fetch, and the owner handle
/// carried rather than re-checked — a listing read is not an owner check, so the
/// last verdict stands (a never-checked follow keeps the handle it was resolved
/// by, exactly as a faulted check would), wherever the follow is homed.
pub fn browse_verdict(
    record: &FollowedFolder,
    previous: Option<&CachedAvailability>,
    (available, display_name): (bool, String),
    now_ms: u64,
) -> CachedAvailability {
    CachedAvailability {
        available,
        display_name,
        owner_handle: owner_handle_verdict::<()>(record.owner_handle.as_deref(), None, previous),
        probed_at_ms: now_ms,
    }
}

/// Resolve availability for every followed record, probing **only the stale
/// ones** and at most [`AVAILABILITY_PROBE_CONCURRENCY`] at a time.
///
/// `cached` is parallel to `records` — the caller's remembered verdict for each,
/// or `None` for never-probed. The returned vec is parallel too, and is what the
/// caller writes back.
///
/// This exists here, rather than in the app glue that calls it, for the reason
/// that glue states about itself: it is built over a concrete `NestClient` and
/// cannot be unit-tested, so the decisions it makes are deliberately not its
/// own. The throttle-vs-latency trade above is exactly such a decision.
pub async fn resolve_availability<R: RpcRequester>(
    nest: &R,
    records: &[FollowedFolder],
    cached: &[Option<CachedAvailability>],
    now_ms: u64,
    ttl_ms: u64,
) -> Vec<CachedAvailability>
where
    R::Error: RpcErrorClass,
{
    use futures_util::StreamExt;

    let stale: Vec<usize> = (0..records.len())
        .filter(|&i| needs_probe(cached.get(i).and_then(Option::as_ref), now_ms, ttl_ms))
        .collect();

    let probed: Vec<(usize, CachedAvailability)> = futures_util::stream::iter(stale)
        .map(|i| async move {
            let record = &records[i];
            // `since` is 0 deliberately: we want the plane's *verdict*, not the
            // page, and the reply's rows are discarded.
            let probe = fetch_followed_changes(nest, record, 0).await;
            let (available, display_name) = availability_from_probe(probe, record);
            // The owner check rides the same budget.
            let verified = match record.owner_handle.as_deref() {
                Some(handle) if owner_handle_checkable(record) && !handle.is_empty() => {
                    Some(verify_owner_handle(nest, handle, &record.owner_actor_id).await)
                }
                _ => None,
            };
            let owner_handle = owner_handle_verdict(
                record.owner_handle.as_deref(),
                verified,
                cached.get(i).and_then(Option::as_ref),
            );
            (
                i,
                CachedAvailability {
                    available,
                    display_name,
                    owner_handle,
                    probed_at_ms: now_ms,
                },
            )
        })
        .buffer_unordered(AVAILABILITY_PROBE_CONCURRENCY)
        .collect()
        .await;

    let mut out: Vec<Option<CachedAvailability>> = cached
        .iter()
        .cloned()
        .chain(std::iter::repeat(None))
        .take(records.len())
        .collect();
    for (i, verdict) in probed {
        out[i] = Some(verdict);
    }
    // Anything still `None` was neither cached nor probed, which cannot happen —
    // `needs_probe(None, ..)` is always true — but fall back to the stored name
    // and available rather than panicking on an invariant a caller could break
    // by passing a short `cached`.
    out.into_iter()
        .enumerate()
        .map(|(i, entry)| {
            entry.unwrap_or_else(|| CachedAvailability {
                available: true,
                display_name: records[i].display_name.clone(),
                owner_handle: None,
                probed_at_ms: now_ms,
            })
        })
        .collect()
}

/// The follower's keyless byte read — moved to the dependency floor
/// ([`fauna_core::file_download::download_followed_file`], which owns the full
/// contract and the ⚠ never-pass-a-key paragraph) so the Media machine's
/// followed browse scope can call it; re-exported here because this module is
/// where every other follow mechanism lives.
pub use fauna_core::file_download::download_followed_file;

/// The listing row [`listing_from_changes`] folds into — defined at the
/// dependency floor so the Media machine can consume it (the
/// [`fauna_core::folder_keys::ResolvedFolderKeys`] pattern: type in
/// `fauna-core`, producer up-stack; this crate cannot be a
/// `fauna-media-machine` dependency because its `mls` feature already depends
/// the other way).
pub use fauna_core::followed_media::FollowedFileEntry;

/// Fold one page of a followed folder's public change log into the current
/// file listing: **the latest change per path wins, and a path whose head is a
/// delete is not listed.**
///
/// The public plane serves raw change rows above the floor
/// (`bins/fauna-nest/src/folder_public.rs`) — it is a *log*, not a listing —
/// and no other fold of it exists client-side, so this is the one place the
/// head rule lives. Pure and transport-free, like everything else in this
/// module that carries a rule.
///
/// The fold's skip rules, each load-bearing:
///
/// * **"Latest" means highest `seq`**, never page position — a caller may
///   concatenate pages or receive rows in any order.
/// * **Retention rows are invisible** (`is_retention` — nest-minted
///   loser-retention vehicles): the contract on the field says receivers
///   account them and *never adopt* them, so one must not shadow the real head
///   here either.
/// * **Non-file rows are invisible** (`entry.is_some()`, or an `item_class`
///   other than the chunk-manifest one): the public plane serves ordinary
///   folders, but the strip does not re-class rows, so fold defensively.
/// * **An unrenderable head is skipped, not an error**: a non-delete head with
///   no plaintext `path` (possible on rows recorded before the sealed-paths
///   expand wrote plaintext alongside) or no `manifest_hash` cannot be listed
///   or fetched by a keyless follower; skipping degrades exactly like the
///   sealed-label render's omit arm, and the row heals on its next change.
///
/// Output is in stable `path` order — the render default; the Media machine
/// re-sorts by the active sort key.
pub fn listing_from_changes(changes: &[SyncChange]) -> Vec<FollowedFileEntry> {
    // The wire spelling `ItemClass::ChunkManifest` carries; an ordinary file
    // row (what `folder_public.rs` writes today) carries `None`.
    const FILE_CLASS: &str = "chunk-manifest";

    let mut heads: std::collections::HashMap<&str, &SyncChange> = std::collections::HashMap::new();
    for c in changes {
        if c.is_retention == Some(true) {
            continue;
        }
        if c.entry.is_some()
            || matches!(c.item_class.as_deref(), Some(class) if class != FILE_CLASS)
        {
            continue;
        }
        match heads.entry(c.path_hash.as_str()) {
            std::collections::hash_map::Entry::Occupied(mut slot) if slot.get().seq < c.seq => {
                slot.insert(c);
            }
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert(c);
            }
            _ => {}
        }
    }

    let mut out: Vec<FollowedFileEntry> = heads
        .into_values()
        .filter(|head| head.change_type != "delete")
        .filter_map(|head| {
            let path = head.path.clone()?;
            let manifest_hash = head.manifest_hash.clone()?;
            Some(FollowedFileEntry {
                path,
                path_hash: head.path_hash.clone(),
                manifest_hash,
                size_bytes: head.size_bytes,
                updated_at: head.created_at,
                thumbnail_hash: head.thumbnail_hash.clone(),
                seq: head.seq,
            })
        })
        .collect();
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_testkit::block_on;
    use fauna_protocol::RpcError;
    use std::sync::Mutex;

    /// A minimal file change row — the fold's fixtures vary only what each
    /// test is about (`..Default::default()`, the fixture-shape rule).
    fn change(seq: i64, path: &str, change_type: &str) -> SyncChange {
        SyncChange {
            seq,
            path_hash: format!("hash-of-{path}"),
            manifest_hash: (change_type != "delete").then(|| format!("manifest-{path}-{seq}")),
            size_bytes: 10 * seq,
            change_type: change_type.to_string(),
            created_at: 1_000 + seq,
            path: Some(path.to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn the_latest_change_per_path_wins() {
        let listing =
            listing_from_changes(&[change(1, "a.jpg", "create"), change(2, "a.jpg", "modify")]);
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].manifest_hash, "manifest-a.jpg-2");
        assert_eq!(listing[0].updated_at, 1_002);
        assert_eq!(listing[0].seq, 2);
    }

    #[test]
    fn latest_means_highest_seq_not_page_position() {
        // Pages concatenated out of order must fold identically.
        let listing =
            listing_from_changes(&[change(2, "a.jpg", "modify"), change(1, "a.jpg", "create")]);
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].seq, 2);
    }

    #[test]
    fn a_deleted_head_is_not_listed() {
        let listing =
            listing_from_changes(&[change(1, "a.jpg", "create"), change(2, "a.jpg", "delete")]);
        assert!(listing.is_empty());
    }

    #[test]
    fn a_recreation_after_a_delete_is_listed() {
        let listing = listing_from_changes(&[
            change(1, "a.jpg", "create"),
            change(2, "a.jpg", "delete"),
            change(3, "a.jpg", "create"),
        ]);
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].manifest_hash, "manifest-a.jpg-3");
    }

    #[test]
    fn a_retention_row_never_shadows_the_real_head() {
        let mut retention = change(3, "a.jpg", "modify");
        retention.is_retention = Some(true);
        let listing = listing_from_changes(&[change(2, "a.jpg", "modify"), retention]);
        assert_eq!(listing.len(), 1);
        assert_eq!(
            listing[0].seq, 2,
            "the retention row must be invisible to the fold"
        );
    }

    #[test]
    fn non_file_rows_are_invisible() {
        let mut state_entry = change(2, "a.jpg", "create");
        state_entry.item_class = Some("state-entry".to_string());
        let mut inline_entry = change(3, "b.jpg", "create");
        inline_entry.entry = Some(fauna_protocol::ByteBuf::from(vec![1u8]));
        let listing =
            listing_from_changes(&[change(1, "a.jpg", "create"), state_entry, inline_entry]);
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].seq, 1);
    }

    #[test]
    fn an_unrenderable_head_is_skipped_not_an_error() {
        let mut pathless = change(1, "a.jpg", "create");
        pathless.path = None;
        let mut manifestless = change(1, "b.jpg", "create");
        manifestless.manifest_hash = None;
        let listing = listing_from_changes(&[pathless, manifestless, change(1, "c.jpg", "create")]);
        assert_eq!(listing.len(), 1);
        assert_eq!(listing[0].path, "c.jpg");
    }

    #[test]
    fn the_listing_is_path_ordered() {
        let listing = listing_from_changes(&[
            change(1, "zebra.jpg", "create"),
            change(2, "alpha.jpg", "create"),
        ]);
        let paths: Vec<&str> = listing.iter().map(|e| e.path.as_str()).collect();
        assert_eq!(paths, ["alpha.jpg", "zebra.jpg"]);
    }

    #[test]
    fn the_entry_carries_the_head_rows_fields() {
        let mut row = change(4, "a.jpg", "modify");
        row.thumbnail_hash = Some("thumb-4".to_string());
        let listing = listing_from_changes(&[row]);
        assert_eq!(
            listing,
            vec![FollowedFileEntry {
                path: "a.jpg".to_string(),
                path_hash: "hash-of-a.jpg".to_string(),
                manifest_hash: "manifest-a.jpg-4".to_string(),
                size_bytes: 40,
                updated_at: 1_004,
                thumbnail_hash: Some("thumb-4".to_string()),
                seq: 4,
            }]
        );
    }

    /// A transport error that can carry a server rejection, so the fold in
    /// [`classify`] can be exercised for real rather than asserted about.
    #[derive(Debug)]
    enum TestError {
        Transport,
        Rejected(RpcError),
    }
    impl std::fmt::Display for TestError {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Self::Transport => write!(f, "transport fault"),
                Self::Rejected(e) => write!(f, "{}", e.code),
            }
        }
    }
    impl RpcErrorClass for TestError {
        fn is_rejection(&self) -> bool {
            matches!(self, Self::Rejected(_))
        }
        fn as_rpc_error(&self) -> Option<&RpcError> {
            match self {
                Self::Rejected(e) => Some(e),
                Self::Transport => None,
            }
        }
    }

    fn rpc_error(code: &str) -> RpcError {
        RpcError::new(code, "error.folders.not_found")
            .with_details_text("no public folder at that address")
    }

    /// What the fake answers for `fauna.actor.by_handle`.
    enum HandleAnswer {
        /// Every handle resolves to this actor id.
        Resolves(String),
        /// Every handle is rejected `fauna.actor.not_found`.
        Unknown,
        /// The read faults (a dropped connection).
        Fault,
    }

    /// A nest that answers one canned outcome and records what it was asked.
    struct FakeNest {
        outcome: Mutex<Option<TestError>>,
        reply: FoldersPublicFetchReply,
        seen: Mutex<Vec<FoldersPublicFetchRequest>>,
        handles: HandleAnswer,
        handles_asked: Mutex<Vec<String>>,
    }
    impl FakeNest {
        fn serving(reply: FoldersPublicFetchReply) -> Self {
            Self {
                outcome: Mutex::new(None),
                reply,
                seen: Mutex::new(Vec::new()),
                handles: HandleAnswer::Unknown,
                handles_asked: Mutex::new(Vec::new()),
            }
        }
        fn refusing(err: TestError) -> Self {
            Self {
                outcome: Mutex::new(Some(err)),
                reply: FoldersPublicFetchReply::default(),
                seen: Mutex::new(Vec::new()),
                handles: HandleAnswer::Unknown,
                handles_asked: Mutex::new(Vec::new()),
            }
        }
        fn answering_handles(mut self, answer: HandleAnswer) -> Self {
            self.handles = answer;
            self
        }
    }
    impl RpcRequester for FakeNest {
        type Error = TestError;
        async fn request<Req, Reply>(
            &self,
            kind: &'static str,
            payload: Req,
        ) -> Result<Reply, Self::Error>
        where
            Req: serde::Serialize,
            Reply: serde::de::DeserializeOwned,
        {
            let bytes = fauna_protocol::encode_canonical(&payload).expect("encode");
            if kind == "fauna.actor.by_handle" {
                let req: fauna_protocol::discovery::ActorByHandleRequest =
                    fauna_protocol::decode_strict(&bytes).expect("decode by_handle");
                self.handles_asked.lock().unwrap().push(req.handle.clone());
                let actor_id = match &self.handles {
                    HandleAnswer::Resolves(id) => id.clone(),
                    HandleAnswer::Unknown => {
                        return Err(TestError::Rejected(rpc_error("fauna.actor.not_found")));
                    }
                    HandleAnswer::Fault => return Err(TestError::Transport),
                };
                let reply = fauna_protocol::discovery::ActorByHandleReply {
                    actor_id,
                    handle: req.handle,
                    domain: "nest.example".to_string(),
                    addresses: Vec::new(),
                    addressable: true,
                    extra: Default::default(),
                };
                let out = fauna_protocol::encode_canonical(&reply).expect("encode reply");
                return Ok(fauna_protocol::decode_strict(&out).expect("decode reply"));
            }
            assert_eq!(kind, "fauna.folders.public.fetch");
            self.seen
                .lock()
                .unwrap()
                .push(fauna_protocol::decode_strict(&bytes).expect("decode request"));
            if let Some(err) = self.outcome.lock().unwrap().take() {
                return Err(err);
            }
            let out = fauna_protocol::encode_canonical(&self.reply).expect("encode reply");
            Ok(fauna_protocol::decode_strict(&out).expect("decode reply"))
        }
    }

    fn served_reply() -> FoldersPublicFetchReply {
        FoldersPublicFetchReply {
            folder_id: 42,
            name: "site".to_string(),
            home_nest_actor_id: Some("ab".repeat(32)),
            changes: Vec::new(),
            extra: Default::default(),
        }
    }

    /// First contact pins what the follow needs: the stable id, the nest stamp,
    /// and the owner — addressed by NAME on the way out.
    #[test]
    fn first_contact_addresses_by_name_and_pins_the_id() {
        let nest = FakeNest::serving(served_reply());
        let page = block_on(resolve_public_folder(
            &nest,
            "https://peer.example",
            &"cd".repeat(32),
            "site",
            0,
        ))
        .expect("serves");

        assert_eq!(page.record.folder_id, 42, "the stable id is pinned");
        assert_eq!(page.record.display_name, "site");
        assert_eq!(page.record.home_nest_url, "https://peer.example");
        assert_eq!(page.record.home_nest_actor_id, Some("ab".repeat(32)));
        assert_eq!(page.record.owner_actor_id, "cd".repeat(32));

        let seen = nest.seen.lock().unwrap();
        assert_eq!(seen[0].folder_name.as_deref(), Some("site"));
        assert_eq!(
            seen[0].folder_id, None,
            "first contact has nothing to pin yet"
        );
        assert_eq!(seen[0].nest_url.as_deref(), Some("https://peer.example"));
    }

    /// Every later read addresses by the PINNED id and stops sending the name —
    /// that is what makes the follow survive a rename.
    #[test]
    fn later_reads_address_by_the_pinned_id_only() {
        let nest = FakeNest::serving(served_reply());
        let followed = FollowedFolder {
            home_nest_url: "https://peer.example".to_string(),
            home_nest_actor_id: Some("ab".repeat(32)),
            owner_actor_id: "cd".repeat(32),
            owner_handle: Some("alice".to_string()),
            folder_id: 42,
            display_name: "the name we last saw".to_string(),
        };

        let page = block_on(fetch_followed_changes(&nest, &followed, 7)).expect("serves");
        // The record refreshes from the reply, so a rename shows up as a new
        // display name rather than a broken follow.
        assert_eq!(page.record.display_name, "site");
        // …while the handle the user followed by rides through: the reply names
        // no owner, so a refreshed record that dropped it would lose the label.
        assert_eq!(page.record.owner_handle.as_deref(), Some("alice"));

        let seen = nest.seen.lock().unwrap();
        assert_eq!(seen[0].folder_id, Some(42));
        assert_eq!(
            seen[0].folder_name, None,
            "the name is not an address any more"
        );
        assert_eq!(seen[0].owner_actor_id, None);
        assert_eq!(seen[0].since, 7);
    }

    /// A same-nest follow sends NO `nest_url`, so the caller's own nest serves
    /// it locally instead of relaying to itself.
    #[test]
    fn a_same_nest_follow_sends_no_relay_url() {
        let nest = FakeNest::serving(served_reply());
        block_on(resolve_public_folder(
            &nest,
            "",
            &"cd".repeat(32),
            "site",
            0,
        ))
        .expect("serves");
        assert_eq!(nest.seen.lock().unwrap()[0].nest_url, None);
    }

    /// The nest's one refusal folds into the one uninformative arm — this is the
    /// revoke, and it must not be distinguishable from "never existed".
    #[test]
    fn the_nests_single_refusal_folds_into_unavailable() {
        let nest = FakeNest::refusing(TestError::Rejected(rpc_error(NOT_FOUND)));
        let followed = FollowedFolder {
            home_nest_url: "https://peer.example".to_string(),
            folder_id: 42,
            ..Default::default()
        };
        assert!(matches!(
            block_on(fetch_followed_changes(&nest, &followed, 0)),
            Err(FollowError::Unavailable)
        ));
    }

    /// A transport fault must NOT fold into `Unavailable`: "the network is down"
    /// and "this folder is no longer public" are different facts, and rendering
    /// them the same would tell a follower their follow was revoked every time
    /// their wifi dropped.
    #[test]
    fn a_transport_fault_is_not_an_unavailable_folder() {
        let nest = FakeNest::refusing(TestError::Transport);
        let followed = FollowedFolder {
            home_nest_url: "https://peer.example".to_string(),
            folder_id: 42,
            ..Default::default()
        };
        assert!(matches!(
            block_on(fetch_followed_changes(&nest, &followed, 0)),
            Err(FollowError::Rpc(TestError::Transport))
        ));
    }

    /// The availability fold, arm by arm. This is the rule the transport glue
    /// would otherwise carry untested.
    #[test]
    fn availability_only_flips_on_the_planes_own_refusal() {
        let stored = FollowedFolder {
            home_nest_url: "https://peer.example".into(),
            home_nest_actor_id: None,
            owner_actor_id: "cd".repeat(32),
            owner_handle: None,
            folder_id: 42,
            display_name: "stored-name".into(),
        };
        let served = PublicFolderPage {
            record: FollowedFolder {
                display_name: "fresh-name".into(),
                ..stored.clone()
            },
            changes: Vec::new(),
        };

        // Served → available, and the name refreshes (a rename shows up).
        let (available, name) = availability_from_probe::<TestError>(Ok(served), &stored);
        assert!(available);
        assert_eq!(name, "fresh-name");

        // The revoke → unavailable, keeping the last known name so the row is
        // still recognisable while the user decides to remove it.
        let (available, name) =
            availability_from_probe::<TestError>(Err(FollowError::Unavailable), &stored);
        assert!(!available);
        assert_eq!(name, "stored-name");

        // A transport fault is NOT an answer about the folder. Flipping here
        // would show "no longer available" on every dropped connection.
        let (available, name) =
            availability_from_probe(Err(FollowError::Rpc(TestError::Transport)), &stored);
        assert!(
            available,
            "a transport fault must never render as a revoked follow"
        );
        assert_eq!(name, "stored-name");

        // …and neither does an unrelated server rejection.
        let (available, _) = availability_from_probe(
            Err(FollowError::Rpc(TestError::Rejected(rpc_error(
                "fauna.protocol.malformed",
            )))),
            &stored,
        );
        assert!(available);
    }

    /// A *different* server rejection stays an Rpc error too — only the plane's
    /// own refusal code means "not public".
    #[test]
    fn an_unrelated_rejection_is_not_folded() {
        let nest = FakeNest::refusing(TestError::Rejected(rpc_error("fauna.protocol.malformed")));
        let followed = FollowedFolder {
            home_nest_url: "https://peer.example".to_string(),
            folder_id: 42,
            ..Default::default()
        };
        assert!(matches!(
            block_on(fetch_followed_changes(&nest, &followed, 0)),
            Err(FollowError::Rpc(TestError::Rejected(_)))
        ));
    }

    // ── Availability staleness budget + bounded probing ───────────────────

    fn followed(id: i64, name: &str) -> FollowedFolder {
        FollowedFolder {
            folder_id: id,
            display_name: name.to_string(),
            ..Default::default()
        }
    }

    fn cached(available: bool, name: &str, at_ms: u64) -> CachedAvailability {
        CachedAvailability {
            available,
            display_name: name.to_string(),
            owner_handle: None,
            probed_at_ms: at_ms,
        }
    }

    // ── The owner handle: remembered, re-verified beside the probe ─────────

    const OWNER: &str = "cdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcdcd";

    fn followed_by(handle: Option<&str>, home_nest_url: &str) -> FollowedFolder {
        FollowedFolder {
            home_nest_url: home_nest_url.to_string(),
            owner_actor_id: OWNER.to_string(),
            owner_handle: handle.map(str::to_string),
            folder_id: 7,
            display_name: "site".to_string(),
            ..Default::default()
        }
    }

    /// The fold, arm by arm — the rule the transport glue would otherwise carry
    /// untested. The load-bearing arm is `Ok(false)`: a handle that now names
    /// someone ELSE must never be painted as this folder's owner.
    #[test]
    fn the_owner_handle_is_shown_only_while_it_still_names_the_owner() {
        let prev_with = CachedAvailability {
            owner_handle: Some("alice".to_string()),
            ..cached(true, "site", 0)
        };
        let prev_without = cached(true, "site", 0);

        assert_eq!(
            owner_handle_verdict::<TestError>(None, Some(Ok(true)), None),
            None,
            "no remembered handle, nothing to show"
        );
        assert_eq!(
            owner_handle_verdict::<TestError>(Some(""), Some(Ok(true)), None),
            None,
            "an empty handle is no handle"
        );
        assert_eq!(
            owner_handle_verdict::<TestError>(Some("alice"), Some(Ok(true)), None).as_deref(),
            Some("alice")
        );
        assert_eq!(
            owner_handle_verdict::<TestError>(Some("alice"), Some(Ok(false)), Some(&prev_with)),
            None,
            "a handle that moved on is dropped even if it verified before"
        );
        // A fault is not an answer: the last verdict stands…
        assert_eq!(
            owner_handle_verdict(
                Some("alice"),
                Some(Err(TestError::Transport)),
                Some(&prev_with)
            )
            .as_deref(),
            Some("alice")
        );
        assert_eq!(
            owner_handle_verdict(
                Some("alice"),
                Some(Err(TestError::Transport)),
                Some(&prev_without)
            ),
            None,
            "a fault never RESURRECTS a handle the last check dropped"
        );
        // …and a never-checked follow keeps the handle it was resolved by.
        assert_eq!(
            owner_handle_verdict(Some("alice"), Some(Err(TestError::Transport)), None).as_deref(),
            Some("alice")
        );
        // No check at all (a cross-nest follow, a browse fetch) is the same
        // non-answer: the follow-time handle stands, and a dropped one stays
        // dropped.
        assert_eq!(
            owner_handle_verdict::<TestError>(Some("bob@peer.example"), None, None).as_deref(),
            Some("bob@peer.example"),
            "a never-checked cross-nest follow keeps the handle it was resolved by"
        );
        assert_eq!(
            owner_handle_verdict::<TestError>(Some("alice"), None, Some(&prev_without)),
            None,
            "no check never RESURRECTS a handle the last check dropped"
        );
    }

    /// The check itself: one `by_handle` read, compared case-insensitively, and
    /// the "no such handle" rejection folded to `false` rather than surfaced.
    #[test]
    fn verify_owner_handle_reads_by_handle_and_folds_the_unresolved_rejection() {
        let owner_upper = FakeNest::serving(served_reply())
            .answering_handles(HandleAnswer::Resolves(OWNER.to_uppercase()));
        assert!(block_on(verify_owner_handle(&owner_upper, "alice", OWNER)).expect("answers"));
        assert_eq!(*owner_upper.handles_asked.lock().unwrap(), ["alice"]);

        let someone_else = FakeNest::serving(served_reply())
            .answering_handles(HandleAnswer::Resolves("ef".repeat(32)));
        assert!(!block_on(verify_owner_handle(&someone_else, "alice", OWNER)).expect("answers"));

        let unknown = FakeNest::serving(served_reply()).answering_handles(HandleAnswer::Unknown);
        assert!(!block_on(verify_owner_handle(&unknown, "alice", OWNER)).expect("answers"));

        let faulty = FakeNest::serving(served_reply()).answering_handles(HandleAnswer::Fault);
        assert!(matches!(
            block_on(verify_owner_handle(&faulty, "alice", OWNER)),
            Err(TestError::Transport)
        ));
    }

    /// A remembered `localpart@domain` is checked by its localpart and holds
    /// only while the nest's echoed domain is the typed one (the fake echoes
    /// `nest.example`): the follow-time routing verdict, re-applied. A shape
    /// that is not a Fauna handle verifies false without a read.
    #[test]
    fn verify_owner_handle_checks_a_domain_qualified_handle_by_localpart_and_domain() {
        let nest = FakeNest::serving(served_reply())
            .answering_handles(HandleAnswer::Resolves(OWNER.to_string()));
        assert!(
            block_on(verify_owner_handle(&nest, "alice@NEST.example", OWNER)).expect("answers")
        );
        assert!(
            !block_on(verify_owner_handle(&nest, "alice@other.example", OWNER)).expect("answers")
        );
        assert_eq!(
            *nest.handles_asked.lock().unwrap(),
            ["alice", "alice"],
            "the localpart alone is what this nest resolves"
        );

        assert!(!block_on(verify_owner_handle(&nest, "@user@instance", OWNER)).expect("answers"));
        assert_eq!(
            nest.handles_asked.lock().unwrap().len(),
            2,
            "no read for a non-Fauna shape"
        );
    }

    /// The probe pass carries the verdict: a same-nest follow's handle is checked
    /// and shown; one that now names someone else is dropped; a cross-nest
    /// follow's is not checked (this nest cannot resolve it) and not shown; and a
    /// follow made by actor id spends no `by_handle` read at all.
    #[test]
    fn the_probe_pass_verifies_same_nest_owner_handles_only() {
        let records = vec![
            followed_by(Some("alice"), ""),
            followed_by(Some("bob@peer.example"), "https://peer.example"),
            followed_by(None, ""),
        ];

        let nest = FakeNest::serving(served_reply())
            .answering_handles(HandleAnswer::Resolves(OWNER.to_string()));
        let verdicts = block_on(resolve_availability(
            &nest,
            &records,
            &[None, None, None],
            1_000,
            AVAILABILITY_TTL_MS,
        ));
        let shown: Vec<Option<&str>> = verdicts.iter().map(|v| v.owner_handle.as_deref()).collect();
        assert_eq!(
            shown,
            [Some("alice"), Some("bob@peer.example"), None],
            "a cross-nest follow keeps the handle it was resolved by, unchecked"
        );
        assert_eq!(
            *nest.handles_asked.lock().unwrap(),
            ["alice"],
            "only the same-nest handle costs a read — a foreign one is never re-dialed"
        );

        let moved_on = FakeNest::serving(served_reply())
            .answering_handles(HandleAnswer::Resolves("ef".repeat(32)));
        let verdicts = block_on(resolve_availability(
            &moved_on,
            &records[..1],
            &[None],
            1_000,
            AVAILABILITY_TTL_MS,
        ));
        assert_eq!(verdicts[0].owner_handle, None);
    }

    /// A browse fetch writes availability evidence but is not an owner check:
    /// the last verdict's handle stands — including a DROPPED one, which the
    /// browse must not resurrect — and a never-checked same-nest follow keeps
    /// the handle it was resolved by.
    #[test]
    fn a_browse_fetch_carries_the_owner_verdict_it_did_not_check() {
        let same_nest = followed_by(Some("alice"), "");
        let dropped = cached(true, "site", 0);
        let kept = CachedAvailability {
            owner_handle: Some("alice".to_string()),
            ..cached(true, "site", 0)
        };

        let v = browse_verdict(&same_nest, Some(&dropped), (false, "site".into()), 9);
        assert_eq!(v.owner_handle, None);
        assert!(
            !v.available,
            "the fetch's own availability is what it records"
        );
        assert_eq!(v.probed_at_ms, 9);

        let v = browse_verdict(&same_nest, Some(&kept), (true, "site".into()), 9);
        assert_eq!(v.owner_handle.as_deref(), Some("alice"));

        let v = browse_verdict(&same_nest, None, (true, "site".into()), 9);
        assert_eq!(v.owner_handle.as_deref(), Some("alice"));

        let cross_nest = followed_by(Some("bob@peer.example"), "https://peer.example");
        let v = browse_verdict(&cross_nest, None, (true, "site".into()), 9);
        assert_eq!(
            v.owner_handle.as_deref(),
            Some("bob@peer.example"),
            "a cross-nest follow keeps the handle it was resolved by — never re-checked"
        );
    }

    #[test]
    fn a_never_probed_record_always_needs_one() {
        assert!(needs_probe(None, 0, AVAILABILITY_TTL_MS));
        assert!(needs_probe(None, u64::MAX, AVAILABILITY_TTL_MS));
    }

    #[test]
    fn a_verdict_inside_the_budget_is_reused_and_one_past_it_is_not() {
        let entry = cached(true, "site", 1_000);
        assert!(!needs_probe(Some(&entry), 1_000, AVAILABILITY_TTL_MS));
        assert!(!needs_probe(
            Some(&entry),
            1_000 + AVAILABILITY_TTL_MS - 1,
            AVAILABILITY_TTL_MS
        ));
        // The boundary is inclusive — exactly TTL old is stale.
        assert!(needs_probe(
            Some(&entry),
            1_000 + AVAILABILITY_TTL_MS,
            AVAILABILITY_TTL_MS
        ));
    }

    /// ⚠ A clock that steps BACKWARDS (a laptop waking, an NTP correction) must
    /// re-probe, not treat a future-stamped entry as fresh forever. Without the
    /// saturating subtraction this underflows and the row freezes at whatever it
    /// last said — indefinitely.
    #[test]
    fn a_backwards_clock_re_probes_rather_than_freezing_the_row() {
        let entry = cached(true, "site", 10_000);
        assert!(needs_probe(Some(&entry), 5_000, AVAILABILITY_TTL_MS));
    }

    /// The budget's whole point: a second pass inside the window spends NO round
    /// trips. `FakeNest` records what it was asked, so this counts real requests
    /// rather than asserting about intent.
    #[test]
    fn a_second_pass_inside_the_budget_probes_nothing() {
        let nest = FakeNest::serving(FoldersPublicFetchReply::default());
        let records = vec![followed(1, "alpha"), followed(2, "beta")];

        let first = block_on(resolve_availability(
            &nest,
            &records,
            &[None, None],
            1_000,
            AVAILABILITY_TTL_MS,
        ));
        assert_eq!(first.len(), 2);
        let after_first = nest.seen.lock().unwrap().len();
        assert_eq!(after_first, 2, "a cold pass probes every record");

        let cached_now: Vec<Option<CachedAvailability>> = first.iter().cloned().map(Some).collect();
        let second = block_on(resolve_availability(
            &nest,
            &records,
            &cached_now,
            1_000 + AVAILABILITY_TTL_MS - 1,
            AVAILABILITY_TTL_MS,
        ));
        assert_eq!(
            nest.seen.lock().unwrap().len(),
            after_first,
            "a pass inside the budget must spend no round trips at all"
        );
        assert_eq!(second, first, "and must return the same verdicts");
    }

    /// Past the budget the probes resume — otherwise a revoke would never be
    /// noticed and the row would claim `Following` forever.
    #[test]
    fn past_the_budget_the_probes_resume() {
        let nest = FakeNest::serving(FoldersPublicFetchReply::default());
        let records = vec![followed(1, "alpha")];
        let stale = vec![Some(cached(true, "alpha", 1_000))];

        let _ = block_on(resolve_availability(
            &nest,
            &records,
            &stale,
            1_000 + AVAILABILITY_TTL_MS,
            AVAILABILITY_TTL_MS,
        ));
        assert_eq!(nest.seen.lock().unwrap().len(), 1);
    }

    /// A mixed pass probes exactly the stale subset — not all, not none.
    #[test]
    fn only_the_stale_records_are_probed() {
        let nest = FakeNest::serving(FoldersPublicFetchReply::default());
        let records = vec![
            followed(1, "fresh-one"),
            followed(2, "stale-one"),
            followed(3, "new-one"),
        ];
        let now = 100_000;
        let cached_now = vec![
            Some(cached(true, "fresh-one", now - 1)),
            Some(cached(true, "stale-one", now - AVAILABILITY_TTL_MS)),
            None,
        ];

        let out = block_on(resolve_availability(
            &nest,
            &records,
            &cached_now,
            now,
            AVAILABILITY_TTL_MS,
        ));

        assert_eq!(
            nest.seen.lock().unwrap().len(),
            2,
            "the stale one and the never-probed one, and nothing else"
        );
        assert_eq!(out.len(), 3);
        assert_eq!(
            out[0].probed_at_ms,
            now - 1,
            "the fresh entry is carried through untouched, stamp included"
        );
        assert_eq!(out[1].probed_at_ms, now);
        assert_eq!(out[2].probed_at_ms, now);
    }

    /// The output stays parallel to `records` even when the caller passes a
    /// short `cached` — the vec is indexed by row, so a length mismatch would
    /// silently shift every verdict onto the wrong folder.
    #[test]
    fn the_result_stays_parallel_to_records_given_a_short_cache() {
        let nest = FakeNest::serving(FoldersPublicFetchReply::default());
        let records = vec![
            followed(1, "alpha"),
            followed(2, "beta"),
            followed(3, "gamma"),
        ];
        let out = block_on(resolve_availability(
            &nest,
            &records,
            &[None],
            1_000,
            AVAILABILITY_TTL_MS,
        ));
        assert_eq!(out.len(), 3);
    }

    /// A revoke on one record must not be smeared onto its neighbours by the
    /// out-of-order completion `buffer_unordered` allows — the verdicts are
    /// re-indexed, not zipped in arrival order.
    #[test]
    fn an_out_of_order_completion_keeps_each_verdict_on_its_own_row() {
        let nest = FakeNest::refusing(TestError::Rejected(rpc_error("fauna.folders.not_found")));
        let records = vec![followed(1, "alpha"), followed(2, "beta")];
        let out = block_on(resolve_availability(
            &nest,
            &records,
            &[None, None],
            1_000,
            AVAILABILITY_TTL_MS,
        ));
        // `FakeNest::refusing` takes its canned error once, so exactly one row
        // sees the revoke and the other sees a serving reply. Whichever way they
        // land, each row keeps ITS OWN name.
        assert_eq!(out.len(), 2);
        for (row, verdict) in records.iter().zip(&out) {
            if !verdict.available {
                assert_eq!(
                    verdict.display_name, row.display_name,
                    "a revoked row keeps its own stored name"
                );
            }
        }
        assert_eq!(
            out.iter().filter(|v| !v.available).count(),
            1,
            "exactly one refusal was canned, so exactly one row is unavailable"
        );
    }
}
