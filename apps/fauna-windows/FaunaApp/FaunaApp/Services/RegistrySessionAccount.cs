using System;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using uniffi.fauna_launch_machine;

namespace FaunaApp.Services;

/// <summary>
/// <see cref="ISessionAccount"/> over the one credential store's registry
/// (<see cref="CredentialStore.Registry"/>). Holds no identity material: every
/// property opens a registry view, resolves the served account, and reads — so
/// there is no copy for a switch or a sign-in to leave stale.
///
/// <para><b>Which account.</b> The one this window is serving right now, as the
/// app shell reports it (<c>servedActor</c> — the loaded crypto identity, the
/// same answer <c>App</c>'s switch guard trusts for "who am I actually serving");
/// before any identity is loaded, the registry's <c>SessionAccount()</c> (the
/// instance holder, else the active account). The live identity comes first
/// because it is what every login path — the launch machine, the wizard's
/// terminal, a switch, the e2e session door — actually moves, whereas the
/// instance holder is re-acquired only at launch and switch.</para>
///
/// <para>The resume rows are read through the same <c>LaunchPersistence</c>
/// shape the launch machine routes on — the bound adapter for a bound secondary
/// instance, the active-account adapter otherwise — so a record the launch flow
/// seeds the wizard with is the record the machine branched on.</para>
/// </summary>
internal sealed class RegistrySessionAccount : ISessionAccount
{
    private readonly Func<string?> _servedActor;

    internal RegistrySessionAccount(Func<string?> servedActor)
    {
        _servedActor = servedActor;
    }

    private FfiSessionMaterial? Material()
    {
        using var registry = CredentialStore.Registry();
        var actor = _servedActor() ?? registry.SessionAccount();
        return actor is null ? null : registry.SessionMaterial(actor);
    }

    private static T? FromLaunchPersistence<T>(Func<LaunchPersistence, T?> read) where T : class
    {
        using var registry = CredentialStore.Registry();
        var persistence = SessionInstance.LaunchBinding is string bound
            ? registry.BoundLaunchPersistence(bound)
            : registry.LaunchPersistence();
        try
        {
            return read(persistence);
        }
        finally
        {
            (persistence as IDisposable)?.Dispose();
        }
    }

    public string? SecretHex => Material()?.@secretHex;
    public string? NestUrl => Material()?.@nestUrl;
    /// <summary>
    /// Persisted-or-derived through the shared get-or-create
    /// (<see cref="SessionDeviceId.Resolve"/>), never the raw per-actor slot.
    /// </summary>
    public string? DeviceId
    {
        get
        {
            using var registry = CredentialStore.Registry();
            var actor = _servedActor() ?? registry.SessionAccount();
            return actor is null ? null : SessionDeviceId.Resolve(registry, CredentialStore.Logical, actor);
        }
    }
    public string? Handle => Material()?.@handle;
    public string? Domain => Material()?.@domain;
    public string? Tier => Material()?.@tier;

    public PendingInviteRecord? PendingInvite => FromLaunchPersistence(p => p.LoadPendingInvite());
    public AwaitingDnsRecord? AwaitingDns => FromLaunchPersistence(p => p.LoadAwaitingDns());
    public PendingFactoryResetRecord? PendingFactoryReset => FromLaunchPersistence(p => p.LoadPendingFactoryReset());
}
