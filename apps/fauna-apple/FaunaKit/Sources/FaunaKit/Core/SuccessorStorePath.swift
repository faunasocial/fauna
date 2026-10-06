import Foundation

/// Apple's `db_path_for` — the per-account MLS store resolver the succession
/// ceremony invokes for the **successor** identity
/// (`docs/goal/behavior/identity-succession.md` § Implementation status today).
///
/// ## Why this is a callback and not a `String`
///
/// The ceremony takes a *resolver*, never an eager path, and that is
/// load-bearing rather than stylistic: resolving an account's directory here
/// **writes** — `AccountStateDir.mlsDbPath` creates the scoped directory. The
/// pinned ordering is that *an unreachable
/// nest must fail before anything is written*, so the shared ceremony calls this
/// only **after** the successor's `connect()` succeeds. Handing it a
/// pre-computed path would run that write unconditionally, including on the arm
/// where the succession never lands — which is exactly the shape
/// `libs/fauna-ffi/src/recovery.rs` makes unrepresentable by taking a
/// `FfiSuccessorStorePath` instead of a string. tui's equivalent is
/// `settings::successor_db_path`.
///
/// ## Why the successor's store is a different file
///
/// Both engines are live at once during the post-succession group sweep — the
/// old identity's, to remove its leaf, and the successor's, to add it — and MLS
/// holds one engine per `mls_state.db`. `AccountStateDir.mlsDbPath` is per-actor
/// (`<base>/<actor>/mls.db`), so a successor whose actor id differs from the
/// predecessor's necessarily lands on a different file. Nothing here has to
/// enforce that; it follows from the layout.
public final class SuccessorStorePath: FfiSuccessorStorePath {
    public init() {}

    /// Resolve (creating the directory as the layout requires) the successor's
    /// MLS store path.
    ///
    /// Never throws and never returns an empty string: a malformed hex resolves
    /// under `-unresolved-` exactly as every other `AccountStateDir` caller
    /// does, because a ceremony that has already re-pointed the account must not
    /// fail over a directory name.
    public func mlsDbPath(successorActorHex: String) -> String {
        AccountStateDir.mlsDbPath(actorIdHex: successorActorHex)
    }
}

/// The retired identity's own MLS store, for
/// `succession_retry_group_sweep`'s `old_store_path`
/// (`docs/goal/ui/settings.md` § Recovery kit → *Finishing an unfinished
/// group sweep*) — the retry's OTHER resolver, and deliberately not this
/// file's `SuccessorStorePath`.
///
/// ⚠ **Must be the PURE scope resolution, never the creating one.** The retry
/// turns on whether the retired identity's store already exists on this
/// device; `SuccessorStorePath.mlsDbPath` creates the directory as a side
/// effect of asking, which would make an untouched retired identity read as a clean
/// empty run while the thief's leaf sits untouched in every real group
/// (`libs/fauna-ffi/src/recovery.rs`'s warning on the same parameter).
public final class RetiredIdentityStorePath: FfiSuccessorStorePath {
    public init() {}

    /// Resolve the retired identity's MLS store path — reads only, never
    /// creates the directory. A nil/malformed hex resolves under
    /// `-unresolved-`, a path no store is written to by this identity, so a
    /// refused hex answers "no old state" rather than naming another account's
    /// live store.
    public func mlsDbPath(successorActorHex: String) -> String {
        AccountStateDir.pureMlsDbPath(actorIdHex: successorActorHex)
    }
}
