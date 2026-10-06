using System;
using System.Threading;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The per-(OS login, account) raise channel's mechanics
/// (<c>account-scoping.md</c> § Concurrent instances → <i>The per-(OS login,
/// account) raise channel</i>).
///
/// <para>These run headless on purpose. The channel is named-kernel-object
/// plumbing, not a window: <c>TryRaise</c> in this process addresses the endpoint
/// exactly the way a colliding <i>process</i> does — by deriving the name — so a
/// same-process raise exercises the identical code path a second FaunaApp.exe
/// takes. The one part that genuinely needs a window (does it come to the front?)
/// is the callback, and it arrives injected.</para>
///
/// <para>They also touch real FFI: <see cref="SessionInstance.InstanceToken"/> crosses
/// into <c>fauna_client_accounts::account_instance_token</c>, which is the point — a C#
/// re-derivation of the token would pass a mocked test and still address the wrong
/// name in production.</para>
/// </summary>
public class AccountActivationEndpointTests
{
    // A fresh well-formed actor id per call. Endpoint names are machine-global
    // within the logon session, so a fixed id would collide with a concurrent
    // test run elsewhere on this machine (the HKCU-race lesson). Two "N"-format
    // GUIDs are exactly 64 lowercase hex chars — the shape the shared derivation
    // requires.
    private static string Actor() =>
        Guid.NewGuid().ToString("N") + Guid.NewGuid().ToString("N");

    /// <summary>Generous budget, deadline-polled — a green run pays only the real
    /// signal latency (e2e conventions point 14, applied to a unit test).</summary>
    private static bool WaitFor(Func<bool> cond, double budgetS = 10.0)
    {
        var deadline = DateTime.UtcNow.AddSeconds(budgetS);
        while (DateTime.UtcNow < deadline)
        {
            if (cond()) return true;
            Thread.Sleep(20);
        }
        return cond();
    }

    /// <summary>
    /// Does the endpoint name still exist? — <b>without signalling it</b>.
    ///
    /// <para>⚠ This is not a stylistic alternative to polling
    /// <c>!TryRaise(actor)</c>, and using <c>TryRaise</c> here silently destroys the
    /// release assertions. <c>TryRaise</c> <i>Sets</i> the event, which wakes the
    /// very listener whose failure-to-wake is the bug under test — the probe
    /// performs the release it is supposed to be observing, and a naive
    /// <c>Dispose()</c>-based release passes. Measured, not reasoned: planting
    /// exactly that release left all seven of these tests green until the probe
    /// stopped signalling.</para>
    /// </summary>
    private static bool EndpointExists(string keyToken)
    {
        if (!EventWaitHandle.TryOpenExisting(AccountActivationEndpoint.NameFor(keyToken), out var ev))
        {
            return false;
        }
        ev.Dispose();
        return true;
    }

    /// <summary>
    /// The core contract: a served account is reachable, and the activation lands.
    /// </summary>
    [Fact]
    public void AServedAccount_IsReachable_AndTheActivationLands()
    {
        var actor = Actor();
        var raised = new ManualResetEventSlim(false);
        using var endpoint = new AccountActivationEndpoint(() => raised.Set());

        // Nobody serves it yet — and this negative is asserted BEFORE the claim, so
        // a permanently-true TryRaise (e.g. one that created the event instead of
        // opening it) cannot make the positive below vacuous.
        Assert.False(AccountActivationEndpoint.TryRaise(actor),
            "an unserved account must have no endpoint to open");

        endpoint.Serve(actor);
        Assert.True(AccountActivationEndpoint.TryRaise(actor),
            "a served account's endpoint must be openable by a would-be raiser");
        Assert.True(raised.Wait(TimeSpan.FromSeconds(10)),
            "the activation must reach the serving instance's callback");
    }

    /// <summary>
    /// The endpoint is keyed on the ACCOUNT, not the install: serving one account
    /// must not make another look reachable. This is the property that makes
    /// several instances under one OS login coherent at all.
    /// </summary>
    [Fact]
    public void ServingOneAccount_DoesNotMakeAnotherReachable()
    {
        var served = Actor();
        var other = Actor();
        using var endpoint = new AccountActivationEndpoint(() => { });

        endpoint.Serve(served);

        Assert.True(AccountActivationEndpoint.TryRaise(served));
        Assert.False(AccountActivationEndpoint.TryRaise(other),
            "an endpoint is keyed on the account — serving one must not answer for another");
    }

    /// <summary>
    /// A switch retargets: the incoming account becomes reachable and the outgoing
    /// one stops being.
    ///
    /// <para>⚠ The release half is the subtle one and the reason this test exists.
    /// <c>WaitOne</c> holds a <c>DangerousAddRef</c> on its handle, so
    /// <c>Dispose()</c>ing an infinitely-waiting listener does NOT close the OS
    /// handle — the process would keep owning the old account's name for its whole
    /// life, and a raiser aiming at the account it no longer serves would get a
    /// delivered-looking raise into the wrong window. Only waking the listener frees
    /// the name.</para>
    ///
    /// <para>Asserted through <see cref="EndpointExists"/>, never
    /// <c>!TryRaise(first)</c> — see that helper for why the obvious spelling makes
    /// this test pass against the very bug it exists to catch.</para>
    /// </summary>
    [Fact]
    public void ASwitch_ClaimsTheIncomingEndpoint_AndReleasesTheOutgoingOne()
    {
        var first = Actor();
        var second = Actor();
        var firstToken = SessionInstance.InstanceToken(first)!;
        using var endpoint = new AccountActivationEndpoint(() => { });

        endpoint.Serve(first);
        Assert.True(EndpointExists(firstToken));

        endpoint.Serve(second);
        Assert.Equal(SessionInstance.InstanceToken(second), endpoint.ServedToken);

        Assert.True(AccountActivationEndpoint.TryRaise(second),
            "the account switched TO must be reachable");
        Assert.True(WaitFor(() => !EndpointExists(firstToken)),
            "the account switched AWAY from must stop being reachable, with nobody " +
            "signalling it — a Dispose against a waiting listener leaves this name " +
            "owned for the process's whole life");
    }

    /// <summary>
    /// A same-account rebuild is a no-op, not a re-claim — and the endpoint keeps
    /// delivering one activation per raise afterwards.
    ///
    /// <para>Sessions rebuild often (every switch back, every re-auth), so a
    /// re-claiming implementation would accumulate handles and listener threads on
    /// one name for the process's whole life.</para>
    /// </summary>
    [Fact]
    public void ASameAccountRebuild_KeepsTheOneEndpoint_AndDeliversOncePerRaise()
    {
        var actor = Actor();
        var raises = 0;
        using var endpoint = new AccountActivationEndpoint(() => Interlocked.Increment(ref raises));

        endpoint.Serve(actor);
        var token = endpoint.ServedToken;
        endpoint.Serve(actor);
        endpoint.Serve(actor.ToUpperInvariant());
        Assert.Equal(token, endpoint.ServedToken);

        Assert.True(AccountActivationEndpoint.TryRaise(actor));
        Assert.True(WaitFor(() => Volatile.Read(ref raises) == 1));
        Assert.True(AccountActivationEndpoint.TryRaise(actor));
        Assert.True(WaitFor(() => Volatile.Read(ref raises) == 2),
            "two raises, two deliveries — no lost or duplicated activation");
    }

    /// <summary>
    /// Disposal releases the name — the clean-exit half of the same property the
    /// switch test pins. A raiser must not be able to "reach" a dead server.
    /// </summary>
    [Fact]
    public void Disposing_ReleasesTheEndpoint()
    {
        var actor = Actor();
        var token = SessionInstance.InstanceToken(actor)!;
        var endpoint = new AccountActivationEndpoint(() => { });
        endpoint.Serve(actor);
        Assert.True(EndpointExists(token));

        endpoint.Dispose();

        Assert.Null(endpoint.ServedToken);
        // Non-signalling probe, same reason as the switch test.
        Assert.True(WaitFor(() => !EndpointExists(token)),
            "a released endpoint must stop answering — liveness IS handle ownership");
    }

    /// <summary>
    /// Degrade-open: an id with no key token yields no endpoint rather than an
    /// exception or a stray name. The instance is then simply unreachable over the
    /// per-account channel, which the raiser reports honestly via the lock re-probe.
    /// </summary>
    [Fact]
    public void AMalformedActorId_ClaimsNothing_AndRaisesNothing()
    {
        using var endpoint = new AccountActivationEndpoint(() => { });

        endpoint.Serve("not-an-actor-id");

        Assert.Null(endpoint.ServedToken);
        Assert.False(AccountActivationEndpoint.TryRaise("not-an-actor-id"));
        Assert.False(AccountActivationEndpoint.TryRaise(""));
    }

    /// <summary>
    /// The name is built from the SHARED instance token, so every spelling of one
    /// account addresses one endpoint. A C#-side re-derivation would pass every
    /// other test in this file and still miss here — which is exactly how a lock
    /// file and an endpoint drift apart.
    /// </summary>
    [Fact]
    public void TheEndpointName_UsesTheSharedInstanceToken_SoSpellingCannotSplitIt()
    {
        var actor = Actor();
        var token = SessionInstance.InstanceToken(actor);
        Assert.NotNull(token);
        Assert.Equal(AccountActivationEndpoint.NamePrefix + token,
            AccountActivationEndpoint.NameFor(token!));

        using var endpoint = new AccountActivationEndpoint(() => { });
        endpoint.Serve(actor.ToUpperInvariant());

        Assert.Equal(token, endpoint.ServedToken);
        Assert.True(AccountActivationEndpoint.TryRaise($"  {actor.ToUpperInvariant()}  "),
            "a padded, upper-cased spelling must reach the same endpoint");
    }
}
