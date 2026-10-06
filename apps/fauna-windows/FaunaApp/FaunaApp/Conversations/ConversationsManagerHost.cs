using System;
using uniffi.fauna_conversations;

namespace FaunaApp.Conversations;

/// <summary>
/// Process-wide accessor for the shared <see cref="ConversationsManager"/>.
/// Mirrors <c>OnboardingViewModel.Current</c> and the singleton pattern in
/// the launch-machine wiring: the manager owns all conversations state and
/// a single instance lives for the lifetime of the app.
///
/// In test-helpers builds (FAUNA_E2E_BRIDGE set) the host registers a mock
/// backend for every rail at construction so <c>inject_inbound_for_test</c>
/// can route inbound messages without the test agent constructing rail
/// backends across the FFI.
///
/// <see cref="Instance"/> is the ONLY <see cref="ConversationsManager"/> for a given
/// identity's session — there is no swap WITHIN a session. When the app builds the
/// real shared-Rust
/// <see cref="ConversationsSession"/> at login, it builds it OVER this same
/// instance (<c>NestRpcClient.BuildConversationsSessionAsync(manager)</c> →
/// <c>FfiNestClient::conversations_session_over_manager</c>), so the real
/// FaunaMls/SMTP rails register onto the manager already held here instead of
/// replacing it. This is deliberate: a manager swap (the old
/// <c>RegisterRealManager(session.Manager())</c> pattern, which published a
/// DIFFERENT manager object built internally by a fresh <c>ConversationsSession</c>)
/// silently orphans anything already injected into the pre-swap instance — the
/// exact defect apple hit and fixed first (<c>testing.md</c> § Cross-app e2e
/// conventions, convention 10). <see cref="MarkRealRailsRegistered"/> only flips a
/// readiness flag now; it never changes which object <see cref="Instance"/> returns.
///
/// <para><b>The actor-change boundary is the ONE sanctioned exception</b>
/// (<see cref="ResetForActorChange"/>, <c>long-term-store.md</c> § Multi-account
/// evolution). The no-swap rule above protects state injected during ONE identity's
/// session; an actor change — a switch, a sign-out or a factory-reset re-onboard — is
/// precisely the point at which the outgoing identity's state must NOT survive. This manager carries that identity's MLS rails, observers
/// and thread store, so reusing it across a switch would have the incoming account
/// encrypting and decrypting its conversations through the SIGNED-OUT identity's
/// rails — a silent cross-identity data bug, the windows-shaped twin of the
/// process-wide MLS-engine hazard the goal doc records for apple.</para>
///
/// <para>Windows' MLS state lives in the per-session <see cref="ConversationsSession"/>
/// built over this host, so replacing this host is windows' seam. (Historically the
/// contrast was with a process-wide <c>ENGINE</c> singleton in <c>libs/fauna-ffi</c>
/// that windows never called; that plane was deleted 2026-07-22 and every app is
/// now on the per-session engine.) Every other app already gives the manager a
/// per-identity lifetime; this brings windows into line rather than inventing a new
/// shape.</para>
///
/// <para><c>clear_for_test()</c> is deliberately NOT that seam: it clears the thread
/// store, drafts and selection but explicitly <i>preserves registered backends and
/// observers</i> (<c>libs/fauna-conversations/src/manager.rs</c>), which are exactly
/// the identity-bound parts a switch has to drop.</para>
/// </summary>
internal static class ConversationsManagerHost
{
    private static readonly object Gate = new();
    private static volatile bool _realRailsRegistered;

    private static Lazy<ConversationsManager> _mockOrEmpty = NewManager();

    /// <summary>
    /// The one page-scoped VM+observer pair for the current <see cref="Instance"/>,
    /// shared across every <c>ConversationsPage</c> load for this manager's lifetime.
    /// <c>ConversationsPage.Page_Loaded</c> reuses this instead of constructing a fresh
    /// pair on every re-navigation, and nulls it here (this class's own reset seam)
    /// makes the next load build a fresh pair against the fresh <see cref="Instance"/>.
    ///
    /// <para>Fixes an observer-accumulation bug:
    /// <c>ConversationsPage</c> carries no
    /// <c>NavigationCacheMode</c> (WinUI's default <c>Disabled</c> applies), so a fresh
    /// page instance — and therefore a fresh <c>Page_Loaded</c> — was built on every
    /// tab/nav-item activation. Each load constructed a fresh
    /// <c>ConversationsNotifyObserver</c> + VM and the VM's constructor unconditionally
    /// called <c>manager.AddObserver(observer)</c>; <c>Page_Unloaded</c> only
    /// unsubscribed the VM's own <c>PropertyChanged</c> C# event, and there is no
    /// <c>manager.RemoveObserver</c> in the shared Rust API
    /// (<c>libs/fauna-conversations/src/manager.rs</c>) for it to call instead — so
    /// every stale page-scoped observer (and the VM/<c>_spamWrite</c> closure it
    /// retained) stayed registered on the manager forever, each firing one dead
    /// UI-thread dispatch per manager emit.</para>
    ///
    /// <para>Safe to reuse across a same-session re-navigation: this host's own class
    /// doc establishes there is no manager swap within one identity's session
    /// (<see cref="Instance"/> and a live <c>ConversationsSession.Manager()</c> wrapper
    /// both address the SAME underlying manager), so the VM's captured manager/
    /// <c>_spamWrite</c> references stay valid for as long as the pair itself does. No
    /// manager-identity check is needed for that reason — nulling this field only at
    /// <see cref="ResetForActorChange"/> is sufficient, and a <c>ReferenceEquals</c>
    /// check would be actively wrong: every UniFFI object crossing the FFI mints a NEW
    /// C# wrapper around the same underlying Rust pointer, so two wrappers for the
    /// identical <c>Arc</c> are never reference-equal in C# (see
    /// <see cref="MarkRealRailsRegistered"/>'s doc comment for the same trap, measured
    /// there as a hang).</para>
    /// </summary>
    internal static (
        FaunaApp.Core.ViewModels.ConversationsViewModel Vm,
        FaunaApp.Conversations.ConversationsNotifyObserver Observer)? PageObserverState;

    private static Lazy<ConversationsManager> NewManager() => new(() =>
    {
        var m = new ConversationsManager();
#if DEBUG || FAUNA_E2E_AGENT
        // testing.md convention 15: `install_mock_backends_for_test` is a UniFFI
        // seam the production FFI flavor does not export, so this call cannot
        // compile in Release — the exact shape android's ConversationsManagerHost
        // needed. FAUNA_E2E_BRIDGE stays the inner runtime switch within a Debug
        // build; `#if DEBUG` is the outer boundary.
        var isE2E = !string.IsNullOrEmpty(Environment.GetEnvironmentVariable("FAUNA_E2E_BRIDGE"));
        if (isE2E)
        {
            m.InstallMockBackendsForTest();
        }
#endif
        // The room-post composer refresh (ui/feed.md § Encryption at rest →
        // Room-restricted — the app half, *The rooms offered*) — registered once per
        // manager, here rather than at any session-build call site, so it covers BOTH
        // meeting points windows has (the production login block and the e2e path's
        // BuildE2eConvSessionAsync) with no risk of double-registration on a same-actor
        // re-entry that reuses this manager (see this host's own class doc — a manager
        // swap WITHIN a session is forbidden, so "once per manager" already means "once
        // per session" and never needs a null-guard field the way the login block's
        // MessageToastObserver does).
        m.AddObserver(new RoomsRefreshObserver());
        // The home-screen widget's feed (apps/windows.md § Home-screen widget): the
        // taskbar badge reads the shared unread fold on every change, for the
        // manager's whole lifetime — same once-per-manager reasoning as the observer
        // above, and the same reset semantics (a fresh manager at an actor change
        // gets a fresh observer that reconciles the badge to the incoming identity).
        m.AddObserver(new UnreadBadgeObserver(m));
        return m;
    });

    /// <summary>
    /// End the outgoing identity's conversations state at an actor change: hand the
    /// conversations-engine role over, drop the manager (with its identity-bound
    /// MLS/SMTP rails, observers and threads) and re-arm the readiness flag so the
    /// incoming identity's <see cref="ConversationsSession"/> registers its own rails
    /// onto a clean one.
    ///
    /// <para>Call this ONLY from an actor-change teardown, never mid-session — see the
    /// class doc for why a within-session swap is forbidden. In practice there is one
    /// caller, <c>App.DropActorScopedState()</c>: until 2026-08-24 this was reached
    /// from the account-switch path alone (hence its former name,
    /// <c>ResetForIdentitySwitch</c>), so a sign-out or factory-reset followed by a
    /// login reused the manager holding the OUTGOING identity's rails, threads and
    /// drafts. Lazy, so the fresh manager is not constructed until something actually
    /// asks for <see cref="Instance"/> afterwards.</para>
    /// </summary>
    public static void ResetForActorChange()
    {
        lock (Gate)
        {
            // Only if something actually asked for it — reading `.Value` on an
            // untouched Lazy would CONSTRUCT a manager just to close it.
            var outgoing = _mockOrEmpty.IsValueCreated ? _mockOrEmpty.Value : null;
            _mockOrEmpty = NewManager();
            _realRailsRegistered = false;
            // The page-scoped VM+observer pair is bound to the OUTGOING manager
            // (`PageObserverState`'s own doc): null it here so the next
            // `ConversationsPage.Page_Loaded` builds a fresh pair against the fresh
            // `Instance` instead of re-registering onto — or leaking a dead observer
            // still pointed at — the manager just discarded above.
            PageObserverState = null;

            // HAND THE ENGINE ROLE OVER FIRST, then close the outgoing manager.
            //
            // Building a session over this host registers the FaunaMls backend ONTO
            // the manager (`libs/fauna-conversations/src/session.rs` — `from_manager`
            // calls `manager.register_backend(fauna_mls.clone())`), so the manager
            // holds an `Arc<FaunaMlsBackend>` → `MlsEngine` → `SqliteStorage` of its
            // own, and this host is process-wide and outlives every session.
            //
            // ⚠ `Dispose()` alone is NOT the hand-over, and this is the seam where
            // that mattered (2026-09-02). Disposing drops ONE reference; the engine's
            // role lock and its open `mls.db` go with the LAST one, and the rest live
            // where no Rust code can see them — the departing `FfiNestClient`'s
            // `scheduling_session` stash, an in-flight build closed on a
            // `ContinueWith`, a receive loop still winding down. Counting those across
            // an FFI boundary "is a race dressed as a lifetime"
            // (`account-data-plane.md` § Multi-instance concurrency), so the ruling's
            // mechanism is an explicit, ordered `retire` instead — and the shared
            // native factory's own retire cannot reach it from here, because the line
            // above has ALREADY swapped `Instance` to a fresh manager: by the time the
            // successor build hands the factory a manager, it is one with no MLS rail,
            // and the factory's retire takes its documented no-op arm.
            //
            // So windows retires at its own seam. Both measured symptoms trace to this
            // one omission: the account-scope erase a few frames later failed with
            // `os error 32` (`mls.db` still open — see `App.RetireConvSession`), and
            // the successor `MlsEngine::new` was refused `ServedElsewhere`, wedging the
            // first conversations command after a relaunch.
            try { outgoing?.RetireConversationsEngine(); } catch { /* teardown is best-effort */ }
            try { outgoing?.Dispose(); } catch { /* teardown is best-effort */ }
        }
    }

    /// <summary>
    /// Record that the real FaunaMls/SMTP rails have been registered onto
    /// <see cref="Instance"/> (by building the session over it — see the class doc).
    /// A readiness-flag flip only; the guarantee that the rails landed on
    /// <see cref="Instance"/> and not a different manager is architectural
    /// (<c>conversations_session_over_manager</c> always builds
    /// <c>ConversationsSession::from_manager</c> over the <c>Arc</c> it was handed,
    /// never a fresh one), not something re-verified here. A prior version of this
    /// method took the built manager and asserted <c>ReferenceEquals</c> against
    /// <see cref="Instance"/> as a runtime double-check — but every UniFFI object
    /// crossing the FFI (including the fresh wrapper a <c>ConversationsSession.Manager()</c>
    /// call returns) mints a NEW C# object around the same underlying Rust pointer, so
    /// two wrappers for the identical <c>Arc</c> are never reference-equal in C#. The
    /// assert therefore fired on every real-conversations e2e activation; a failed
    /// <c>Debug.Assert</c> blocks the calling thread on a native dialog nobody can
    /// click, which silently hung <c>BuildE2eConvSessionAsync</c> forever with no
    /// exception — the root cause behind every real-conversations windows e2e test
    /// (`real_faunamls_app`) having been broken since the fixture was generalized to
    /// windows.
    /// </summary>
    public static void MarkRealRailsRegistered()
    {
        _realRailsRegistered = true;
    }

    /// <summary>
    /// Whether the real wire-backed rails have been registered onto
    /// <see cref="Instance"/> yet. Surfaced to the e2e harness as
    /// <c>data.conv_real_backend_active</c> so <c>actions/conversations.py</c>'s
    /// <c>enable_real_faunamls</c> can poll for readiness — the windows twin of linux's
    /// <c>conv_backend::is_e2e_real_active</c> (which reads <c>ACTIVE_SESSION.ready</c>).
    /// </summary>
    public static bool IsRealManagerRegistered => _realRailsRegistered;

    public static ConversationsManager Instance
    {
        get
        {
            // Read the field under the lock, but force the Lazy OUTSIDE it: the
            // factory constructs a ConversationsManager across the FFI, and running
            // that under the same lock ResetForActorChange takes would turn any
            // future re-entrant Instance access into a deadlock.
            Lazy<ConversationsManager> lazy;
            lock (Gate) { lazy = _mockOrEmpty; }
            return lazy.Value;
        }
    }
}
