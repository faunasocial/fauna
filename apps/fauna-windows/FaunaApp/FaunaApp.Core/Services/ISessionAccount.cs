using uniffi.fauna_launch_machine;

namespace FaunaApp.Core.Services;

/// <summary>
/// The identity material of the account <b>this window serves</b>, read through
/// the multi-account registry on every access — never a cached copy and never a
/// platform-side single slot.
///
/// <para>The registry is the only store (<c>long-term-store.md</c> § Downgrade
/// mirror + abandoned-append recovery, RETIRED 2026-09-24): the account's secret
/// and nest binding are its per-actor slots, its handle/domain/tier the index
/// entry's server-data cache, and the three wizard-resume rows its per-actor
/// launch slots. Every property resolves the session account's
/// <c>SessionMaterial</c> (or, for the resume rows, the same
/// <c>LaunchPersistence</c> the launch machine routes on) at the moment it is
/// read, so a view that reads after a sign-in, a switch or a silent-sign-in
/// cache refresh sees the account as it stands now.</para>
///
/// <para>"The session account" is the account this window is serving right
/// now — never "the active one" by assumption, so a bound secondary instance
/// reads its own account (<c>account-scoping.md</c> § Concurrent instances →
/// "Session identity resolves through the session's account"). The shell's
/// implementation says how it resolves that.</para>
///
/// <para>Read-only by design, so every PROPERTY is a pure read. Every write goes
/// through a shared moment on the registry — <c>ConfirmIdentity</c>,
/// <c>PersistLoggedIn</c>, <c>PersistAwaitingDns</c>, <c>PersistPendingInvite</c>,
/// the launch persistence's <c>SaveAuthenticated</c> — so no app-side write path
/// can diverge from the other six apps'. The one member that may write is a
/// METHOD, <see cref="ResolveDeviceId"/>, so a read can never reach it by
/// accident: it once sat behind a property, the e2e state snapshot polled that
/// property through a wizard run, and an identity the user had abandoned kept a
/// device-id slot that made it a ghost switcher row.</para>
/// </summary>
internal interface ISessionAccount
{
    /// <summary>The account's identity secret (64-hex), or null when no account is in session.</summary>
    string? SecretHex { get; }

    /// <summary>The account's home nest, or null before its <c>LoggedIn</c> terminal recorded one.</summary>
    string? NestUrl { get; }

    /// <summary>The account's per-actor device-id slot as it stands — null until
    /// something minted one, and not necessarily a sync id. For observers; a
    /// consumer that registers or enrolls under the id calls
    /// <see cref="ResolveDeviceId"/>.</summary>
    string? StoredDeviceId { get; }

    /// <summary>The sync device id this account registers under on this install,
    /// through the shared get-or-create (<c>SessionDeviceId.Resolve</c>) — which
    /// PERSISTS a derived id into the account's own slot. Call it only where the id
    /// is about to be used, never from an observer: a slot written on an identity
    /// the wizard has not finished keeps it from being retired (<c>long-term-store.md</c>
    /// § Eager vs. lazy migration at native boot).</summary>
    string? ResolveDeviceId();

    /// <summary>Server-data cache (the index entry): the account's handle.</summary>
    string? Handle { get; }

    /// <summary>Server-data cache (the index entry): the account's domain.</summary>
    string? Domain { get; }

    /// <summary>Server-data cache (the index entry): the account's tier.</summary>
    string? Tier { get; }

    /// <summary>The account's pending-invite resume slot (<c>onboarding.md</c> § The pending-invite surface).</summary>
    PendingInviteRecord? PendingInvite { get; }

    /// <summary>The account's deferred-DNS resume slot (<c>onboarding.md</c> § "Almost ready" surface).</summary>
    AwaitingDnsRecord? AwaitingDns { get; }

    /// <summary>The account's pending-factory-reset resume slot (gap CR-1, <c>nest/common.md</c> § Client-state recoverability).</summary>
    PendingFactoryResetRecord? PendingFactoryReset { get; }
}
