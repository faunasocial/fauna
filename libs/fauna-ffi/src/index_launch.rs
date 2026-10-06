//! Native (apple/windows/android) assembly of the content-index plane — the
//! thin adapter the FFI `conversations_session` factory calls to build
//! [`NestMailIndexLauncher`], the sibling of [`crate::mls_sync_launch`] and the
//! same shape: no UniFFI export, no per-app glue, the whole plane activated
//! from the shared factory (priority #2).
//!
//! The launcher is the **single registrar for both directions** — it is the one
//! object holding the MSEK, so it owns the build side
//! (`IndexBuilderLauncher::launch`, the observer `start_receive_loop` registers
//! before its first poll) and the query side (`local_search_index`, the Search
//! page's local arm). App glue passes a `NestClient` and gets back an opaque
//! observer or an opaque index — **never a key**
//! (`docs/goal/behavior/content-index.md` § Where the index is built; the same
//! posture tui's `conv_backend.rs` + `search.rs` already take).
//!
//! ## Why this build may query but not build — [`CLIENT_BUILDS_INDEX`]
//!
//! `content-index.md` § Where queries run — *Build vs. query is not the same
//! question* rules the two apart: querying a synced index is cheap and every
//! key-holding device does it, but **building** is a steady background workload
//! (decrypt → tokenize → seal → publish), and "iOS's background-execution budget
//! makes *a phone is the builder* unreliable, and Android's is only somewhat
//! better". The ratified model is therefore *a desktop app (or the MDA bridge
//! in-session) builds and syncs; phones receive the segments and query the
//! synced copy* — a phone-only user's foreground-incremental fallback is a named,
//! separate piece of that spec, not this wiring.
//!
//! So the split is a **property of the target, not a user choice** — nobody
//! configures it, and there is no app-supplied flag to get wrong (the
//! one-configuration-surface invariant's bucket 1: a value no human chooses is a
//! constant the binary derives). `fauna-ffi` compiles once per target, so
//! `cfg!(target_os)` *is* that derivation.
//!
//! ## The same constant drives the picker, which is what keeps them honest
//!
//! Whether a client may be **pinned** for the `index` task kind is declared per
//! app, and the 2026-08-03 picker session's finding was that a two-factor
//! approximation of a per-(client, kind) rule rots silently the moment the
//! per-app sets diverge — in *both* directions (a client offering a kind it
//! cannot run, or withholding one it can). Deriving
//! [`crate::task_delegation`]'s `Runner` mapping from this same constant makes
//! builder-and-picker agreement structural rather than a convention two files
//! have to remember.

/// Whether **this build** runs the content-index builder, as opposed to only
/// querying an index another of the user's devices built and synced.
///
/// `true` on the desktop FFI targets (windows, macOS), `false` on the phones
/// (iOS, Android) — the ratified build-vs-query split, module docs above. The
/// apps' own Task-delegation capability declarations already mirror it
/// (`TaskDelegationView.swift`'s `#if os(macOS) .runner #else .viewerOnly`,
/// windows `.Runner`, android `VIEWER_ONLY`), and
/// [`crate::task_delegation`]'s kind mapping now derives from this constant so
/// the two can no longer drift apart.
///
/// A phone still gets the **full local arm**: its Search page queries the synced
/// segments exactly like a desktop's, because
/// [`NestMailIndexLauncher::local_search_index`] is registered unconditionally.
/// What it skips is only the publishing half.
#[cfg(any(feature = "conversations-session", feature = "task-delegation"))]
pub(crate) const CLIENT_BUILDS_INDEX: bool = !cfg!(any(
    target_os = "ios",
    target_os = "android",
    // A phone-shaped simulator/host target is still the phone build.
    target_os = "watchos",
    target_os = "tvos"
));

#[cfg(feature = "conversations-session")]
mod arm {
    use std::sync::{Arc, Mutex};

    use fauna_client::NestClient;
    use fauna_client_conversations::{IndexLeaseSeat, MailKeyCache, NestMailIndexLauncher};
    use fauna_conversations::ConversationsManager;

    /// The two handles the Search page's local arm needs, stashed by the
    /// `conversations_session` factory so a `search_manager()` taken **before**
    /// login still reaches them afterwards — the same late-population shape as
    /// `FfiCaldavClient`'s `scheduling_session` holder.
    ///
    /// Both halves are required and neither is derivable from the other: the
    /// launcher opens the sealed slice (it holds the MSEK), and the manager is
    /// the content lookup a local hit's snippet + `SearchNav` resolve out of,
    /// since the index stores postings only.
    #[derive(Clone)]
    pub(crate) struct IndexArm {
        pub(crate) launcher: Arc<NestMailIndexLauncher>,
        // Read only by the `search-manager` attach (`nest_client.rs`), which a
        // flavor without discovery — the kids complement — excises.
        #[cfg_attr(not(feature = "search-manager"), allow(dead_code))]
        pub(crate) lookup: Arc<ConversationsManager>,
    }

    /// Shared, late-populated handle to this login's index arm.
    pub(crate) type IndexArmHolder = Arc<Mutex<Option<IndexArm>>>;

    /// Build this login's index launcher over `nest` and the session's **shared**
    /// [`MailKeyCache`].
    ///
    /// The cache is passed in rather than minted here on purpose: the inbound
    /// mail read-feeds and the scheduling sink derive the same keys, and
    /// `NestMailInboundSource::inbox_and_sent_over` exists precisely so a login
    /// pays **one** mail-custody load for all of them instead of one each (the
    /// `MailKeyCache` lift the 2026-08-03 lifecycle session took; tui/linux wire
    /// it the same way).
    /// ## The `index`-lease seat is the one input app glue must supply
    ///
    /// A builder heartbeats the advisory `index` lease as a **device**
    /// (`participants.md` § Coordination primitive → *The `index` kind under the
    /// lease*), and a device id is app-owned state: a row in a SQLite file under
    /// a directory the app chooses. `NestClient` does not carry one, and this
    /// factory could derive neither it nor anything equivalent — so unlike the
    /// build/query split above, this genuinely **cannot** be settled here. That
    /// refutes the "zero per-app glue, the row-8 precedent" premise this leg was
    /// scoped under: row 8 needed nothing the factory lacked, and the lease needs
    /// exactly one thing it lacks. Hence `seat`, threaded from
    /// `conversations_session`'s `index_lease_device` parameter (— shape 1, the parameter, because a stash setter's
    /// ordering contract fails *silently*: an uncoordinated builder still indexes
    /// everything, so a forgotten call has no error, no log and no failing test).
    ///
    /// `None` stays a **supported** state, not a stub: the builder runs
    /// uncoordinated byte-for-byte as it did before the lease existed
    /// (`IndexBuilder::lease_gate`) — it indexes everything, it just does not
    /// appear as the runner-of-record and does not stand down for a peer. Overlap
    /// is the safe direction under the `(kind, content_id)` dedup, which is
    /// exactly why the ruling makes the heartbeat additive. A phone passes `None`
    /// and that is the *correct* answer there rather than a concession —
    /// [`CLIENT_BUILDS_INDEX`] is already `false` on iOS/Android, so a phone never
    /// builds and therefore has nothing to coordinate.
    ///
    /// ## The File arm's shared-set key resolver needs no such input
    ///
    /// The two look alike and are not: a lease seat needs a **device id**, which
    /// is app-owned state this factory is handed nothing to derive; the resolver
    /// needs only the actor keypair, which `NestClient::auth()` already carries —
    /// the same source `build_devices_machine`'s own resolver wiring and the
    /// snapshots/sync/folders façades all take it from. So this one costs zero
    /// per-app glue and lights up
    /// all four UniFFI apps at once.
    ///
    /// It must be injected rather than built inside the launcher because
    /// `fauna-client-folders` depends on `fauna-client-conversations` under its
    /// `mls` feature — the launcher cannot depend on it back. The launcher derives
    /// the *owner* root itself from the identity seed, so this covers only sets
    /// shared **with** the actor; without it those rows are skipped **without
    /// burning the re-index guard**, so a seat that does hold the keys stages them
    /// (`content-index.md` § Ingest triggers, v1 → *The files/media arms are
    /// SCOPED*).
    pub(crate) fn index_launcher(
        nest: Arc<NestClient>,
        keys: Arc<MailKeyCache>,
        seat: Option<IndexLeaseSeat>,
    ) -> Arc<NestMailIndexLauncher> {
        let launcher = NestMailIndexLauncher::new(Arc::clone(&nest), keys, seat);
        #[cfg(feature = "folders-author")]
        if nest.auth().keypair().is_some() {
            launcher.set_folder_key_resolver(Arc::new(
                fauna_client_folders::NestFolderKeyResolver::new(
                    Arc::clone(&nest),
                    crate::account_runtime::folder_key_store(),
                ),
            ));
        }
        launcher
    }
}

#[cfg(feature = "conversations-session")]
pub(crate) use arm::{IndexArm, IndexArmHolder, index_launcher};
