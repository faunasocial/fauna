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

    private IConversationsSession? _session;
    private string? _lastApplied;

    public void OnChanged() => Apply();

    public void AttachSession(IConversationsSession session)
    {
        _session = session;
        Apply();
    }

    private void Apply()
    {
        var session = _session;
        if (session is null)
        {
            return;
        }

        var identity = Machine?.Snapshot().identity;
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
