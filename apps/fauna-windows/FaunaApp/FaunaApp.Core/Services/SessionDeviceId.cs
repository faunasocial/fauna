using System;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The sync device id a registered account registers under on this install —
/// what <c>RegistrySessionAccount.DeviceId</c> serves every launch path (the
/// account-store runtime's enrollment target, the conversations session's index
/// lease, the hydration session's provisioning).
///
/// <para>Resolved through the shared get-or-create
/// (<c>FfiAccountRegistry.DeviceIdForActor</c>,
/// <c>sync-agent-credentials.md</c> § Credential model, the RULED 2026-09-20
/// block), never read off the raw per-actor slot: the shared rule serves a
/// stored id only when it is a 32-byte-hex sync id and otherwise derives and
/// persists one — the same rule this app's e2e session door and onboarding
/// terminal already apply (<c>App.xaml.cs</c>'s <c>deviceIdForActor</c> arm,
/// <c>OnboardingViewModel</c>), and apple's <c>FaunaAccounts.deviceId</c>.
/// Serving the slot verbatim let a non-sync value reach the account runtime,
/// whose assembly then refused with "this device has no own device id to
/// enroll on" — no account store mounted for that session.</para>
/// </summary>
internal static class SessionDeviceId
{
    /// <summary>
    /// <paramref name="installStore"/> is windows' one credential store
    /// (<c>CredentialStore.Logical</c>'s own doc says why one store is safe).
    /// Null when the registry does not hold <paramref name="actorId"/>, or when
    /// no stable id exists (the shared call's only error) — the callers' "no
    /// device id" branch, rather than an id the next read could not reproduce.
    /// </summary>
    internal static string? Resolve(FfiAccountRegistry registry, FfiSecretStore installStore, string actorId)
    {
        if (registry.SessionMaterial(actorId) is null) return null;
        try
        {
            return registry.DeviceIdForActor(installStore, actorId);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("SessionDeviceId", $"deviceIdForActor failed; no device id this session: {ex.Message}");
            return null;
        }
    }
}
