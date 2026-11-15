using uniffi.fauna_conversations;
using uniffi.fauna_launch_machine;

namespace FaunaApp.Core.Services;

/// <summary>
/// A <see cref="LaunchObserver"/> that heals the live <see cref="IConversationsSession"/>'s
/// self-address whenever the launch machine's identity resolves or changes — the windows leg
/// of <c>docs/goal/ui/conversations.md</c> § State & data shape → *Self-address: live, never
/// baked*. Mirrors linux's <c>IdentityRefreshed</c> handler / tui's <c>SelfAddressRefreshed</c>
/// arm / android's <c>AppLaunchVM.applyIdentity</c> (the reference native consumer of the same
/// <c>LaunchSnapshot.identity</c> channel).
///
/// <para><see cref="Machine"/> is settable rather than constructor-injected because the
/// <see cref="LaunchMachine"/> it observes takes the observer as a constructor argument — the
/// observer must exist first. Assign it immediately after construction, before
/// <c>LaunchMachine.Start()</c>.</para>
///
/// <para><see cref="AttachSession"/> is called once <c>StartMainAppAsync</c> builds the real
/// session (no session exists yet at launch-machine construction time) and immediately applies
/// whatever identity is already resolved — covering an identity that resolved in the gap
/// between machine construction and session build, which would otherwise be silently dropped
/// (no further <c>OnChanged</c> tick is guaranteed to follow).</para>
/// </summary>
internal sealed class SelfAddressHealObserver : LaunchObserver
{
    public ILaunchMachine? Machine { get; set; }

    /// <summary>
    /// Every change of the observed machine, forwarded raw — on the machine's own
    /// thread, so a subscriber marshals to the UI itself. The launch flow is
    /// otherwise pulled (each entry point is awaited, then one snapshot is
    /// dispatched); the tick exists for the one change no app call starts: the
    /// machine's own refresh at a lock's unlock time, which the locked launch
    /// surface follows out (<c>LockedLaunchViewModel.NoticeHolds</c>; FaunaKit's
    /// <c>LaunchTicks</c> is the twin). This observer is the one the app wires onto
    /// every <see cref="LaunchMachine"/> it builds, so the hook rides it rather than
    /// a second observer at each construction site.
    /// </summary>
    public Action? Ticked { get; set; }

    /// <summary>
    /// Raised (on the machine's own thread) when the published identity's handle, domain or
    /// tier moved — resolved, renamed, a primary-domain rename the verify reply carries, or
    /// the account moved to another tier — so the profile SELF header repaints its
    /// <c>&lt;handle&gt;@&lt;current primary&gt;</c> (<c>mail-primary-domain-rename.md</c>
    /// § Display-handle interaction; linux's <c>repaint_self_headers</c>, tui's
    /// <c>SelfAddressRefreshed</c> push) and the Settings identity cell repaints its address
    /// and tier (<c>settings.md</c> § Layout &amp; flow item 1). The persistence's
    /// <c>SaveAuthenticated</c> has written the cache before the machine notifies, so a
    /// repaint reads the new values. Defaults to the process-wide
    /// <see cref="SelfDisplayHandle.Moved"/> fan-out, so no construction site wires it; it
    /// needs no conversations session.
    /// </summary>
    public Action? IdentityMoved { get; set; } = SelfDisplayHandle.RaiseMoved;

    private IConversationsSession? _session;
    private string? _lastApplied;
    private (string Handle, string Domain, string Tier)? _lastIdentity;

    public void OnChanged()
    {
        var identity = Machine?.Snapshot().identity;
        Apply(identity);
        NoteIdentity(identity);
        Ticked?.Invoke();
    }

    /// <summary>Raise <see cref="IdentityMoved"/> when the snapshot's identity differs from
    /// the last one seen — never on a tick that moved something else (a token refresh).</summary>
    private void NoteIdentity(LaunchIdentity? identity)
    {
        if (identity is null)
        {
            return;
        }

        var now = (identity.handle ?? "", identity.domain ?? "", identity.tier ?? "");
        if (now == _lastIdentity)
        {
            return;
        }

        _lastIdentity = now;
        IdentityMoved?.Invoke();
    }

    public void AttachSession(IConversationsSession session)
    {
        _session = session;
        Apply(Machine?.Snapshot().identity);
    }

    private void Apply(LaunchIdentity? identity)
    {
        var session = _session;
        if (session is null)
        {
            return;
        }

        var handle = identity?.handle ?? "";
        var domain = identity?.domain ?? "";
        // An unresolved half is dropped rather than composed: an empty local part or an empty
        // domain is the forbidden "@nest.example" shape (conversations.md § State & data shape),
        // which must be treated exactly like a missing address — never a half-composed one.
        if (handle.Length == 0 || domain.Length == 0)
        {
            return;
        }

        var selfAddress = $"{handle}@{domain}";
        if (selfAddress == _lastApplied)
        {
            return;
        }

        session.SetSelfAddress(selfAddress);
        _lastApplied = selfAddress;
    }
}
