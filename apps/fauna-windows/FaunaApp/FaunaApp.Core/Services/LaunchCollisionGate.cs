using System;
using System.Collections.Generic;
using System.Linq;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// One offerable row of the launch-collision chooser: an account no live instance
/// currently serves, labelled with the same shared formatter the switcher uses so
/// the two surfaces name an account identically.
/// </summary>
/// <param name="ActorId">Actor id (hex) — what a pick binds this process to.</param>
/// <param name="DisplayLabel">
/// <c>account_display_label(handle, actor_id)</c>: the handle when the server-data
/// cache has one, a short actor-id form before the first refresh.
/// </param>
public sealed record LaunchInstanceChoice(string ActorId, string DisplayLabel);

/// <summary>
/// The pure half of windows' launch-collision surface
/// (<c>docs/goal/architecture/apps/account-scoping.md</c> § Concurrent instances
/// → "the colliding instance's surface"): <b>should this launch render the
/// chooser</b>, and <b>which accounts may it offer</b>.
///
/// <para>Kept here, beside <see cref="SingleInstanceGate"/> and for the same
/// reason: the Win32 mutex, the named events and the WinUI page live in the
/// presentation layer, but the policy they consult must be unit-testable without
/// WinUI types or a live lock file. It is the C# twin of linux's
/// <c>account_scope::choosable_accounts</c> + <c>main.rs::launch_collision_detected</c>.</para>
///
/// <para><b>Both answers are display-only.</b> An account free at probe time can be
/// taken before the human clicks, so arbitration stays where it always was — at
/// <c>SessionInstance.Become</c>. That is why the pick must still go through
/// <c>BindAccount</c> + acquire and handle a refusal rather than trusting this list.</para>
/// </summary>
public static class LaunchCollisionGate
{
    /// <summary>
    /// Does this launch collide — i.e. must it render the chooser instead of
    /// authenticating? Three conditions, all required (linux states them in the
    /// same order, and the order matters):
    ///
    /// <list type="number">
    /// <item><b>A plain launch.</b> A <c>FAUNA_BOUND_ACCOUNT</c> (or chooser-bound)
    /// launch that collides stays terminally refused — "the chooser is strictly a
    /// human affordance: wired IPC must be deterministic". Checked first, so a
    /// bound launch never even probes.</item>
    /// <item><b>A resolvable would-be account.</b> The store-active account is what
    /// a plain launch binds to. None — a fresh install, or an index not yet
    /// materialized — means there is nothing to collide with <i>and</i> nothing to
    /// offer, so the ordinary routing applies.</item>
    /// <item><b>That account is currently served</b>, per the shared display-only
    /// probe.</item>
    /// </list>
    /// </summary>
    /// <param name="launchBinding">
    /// This process's binding, or <c>null</c> for a plain launch. Read through
    /// <c>SessionInstance.LaunchBinding</c>, never the environment.
    /// </param>
    /// <param name="activeActorId">The store-active account, or <c>null</c>.</param>
    /// <param name="isServed">
    /// The display-only probe — normally <c>SessionInstance.IsServed</c>. Injected so
    /// the decision is testable without lock files.
    /// </param>
    public static bool CollidesWithALiveInstance(
        string? launchBinding,
        string? activeActorId,
        Func<string, bool> isServed)
    {
        if (launchBinding is not null)
        {
            return false;
        }
        if (string.IsNullOrWhiteSpace(activeActorId))
        {
            return false;
        }
        if (isServed is null)
        {
            // No probe means no evidence of a collision. Fall through to the
            // ordinary launch: the guard at acquire is still the arbiter, so the
            // worst case is a terminal refusal instead of a chooser — never a
            // second process racing one account's scoped state.
            return false;
        }
        return isServed(activeActorId);
    }

    /// <summary>
    /// Those registry accounts the chooser may offer — every account <b>not</b>
    /// currently served, order preserved, labelled for display.
    ///
    /// <para>The served account is excluded by construction rather than by an
    /// explicit "skip the active one" rule: what makes a row unofferable is that
    /// some live instance holds its lock, and the collided account is exactly such
    /// a row. Any account served by a <i>bound</i> sibling drops out the same way.</para>
    ///
    /// <para>An empty result is a legitimate outcome, not an error — every account
    /// this install knows is already open somewhere. The page renders its
    /// "all open" explanation and its two other exits still work, so the user is
    /// never stranded.</para>
    /// </summary>
    /// <param name="entries">Registry rows as (actorId, handle-or-null) pairs.</param>
    /// <param name="notCurrentlyServed">
    /// The shared probe over a set of actor ids — normally
    /// <c>SessionInstance.NotCurrentlyServed</c>. Injected for the same testability
    /// reason as <paramref name="isServed"/> above.
    /// </param>
    /// <param name="displayLabel">
    /// The shared <c>account_display_label</c> binding, injected so this stays a
    /// pure function (the FFI call itself is pinned by the switcher's tests).
    /// </param>
    public static IReadOnlyList<LaunchInstanceChoice> ChoosableAccounts(
        IEnumerable<(string ActorId, string? Handle)> entries,
        Func<IEnumerable<string>, IReadOnlyList<string>> notCurrentlyServed,
        Func<string?, string, string> displayLabel)
    {
        var rows = entries?.ToArray() ?? Array.Empty<(string ActorId, string? Handle)>();
        if (rows.Length == 0 || notCurrentlyServed is null || displayLabel is null)
        {
            return Array.Empty<LaunchInstanceChoice>();
        }

        var free = notCurrentlyServed(rows.Select(r => r.ActorId))
            ?? (IReadOnlyList<string>)Array.Empty<string>();
        var freeSet = new HashSet<string>(free, StringComparer.OrdinalIgnoreCase);

        return rows
            .Where(r => freeSet.Contains(r.ActorId))
            .Select(r => new LaunchInstanceChoice(r.ActorId, displayLabel(r.Handle, r.ActorId)))
            .ToArray();
    }

    /// <summary>
    /// Resolve the chooser's <b>focus-existing</b> exit for
    /// <paramref name="servedActorId"/> — the ratified best-effort raise plus its
    /// honest degrade (<c>account-scoping.md</c> § Concurrent instances → <i>The
    /// per-(OS login, account) raise channel</i>): try the account's activation
    /// endpoint; if it did not answer, <b>re-probe the lock</b>, because an unowned
    /// endpoint means either "the sibling is gone" (proceed) or "alive but
    /// endpoint-less" (tell the user).
    ///
    /// <para>The decision is shared Rust — <c>fauna_client_accounts::resolve_focus_existing</c>,
    /// the one tui and linux route through — reached over UniFFI; this method only
    /// adapts the two platform hooks into its <see cref="FfiFocusExistingSeat"/>.</para>
    /// </summary>
    /// <param name="servedActorId">The account the running instance serves.</param>
    /// <param name="tryRaise">
    /// Send the platform activation to that account's endpoint; <c>false</c> ⇒ the
    /// endpoint is unowned. Normally <c>SingleInstanceManager.TryRaiseAccountInstance</c>.
    /// </param>
    /// <param name="isServed">
    /// The shared display-only lock probe — normally <c>SessionInstance.IsServed</c>.
    /// </param>
    internal static FfiFocusExistingOutcome ResolveFocusExisting(
        string servedActorId,
        Func<string, bool>? tryRaise,
        Func<string, bool>? isServed) =>
        FaunaFfiMethods.ResolveFocusExisting(
            servedActorId ?? string.Empty,
            new FocusExistingSeat(tryRaise, isServed));

    /// <summary>
    /// The seat's hooks as the shared decision's callback interface. Both degrade
    /// toward the <b>non-claiming</b> answer when they cannot tell — a missing or
    /// throwing raise reads as "unowned", a missing or throwing probe as "still
    /// served" — because arbitration is at acquire, so the worst case must be a
    /// surfaced message rather than a second process racing one account's state.
    /// </summary>
    private sealed class FocusExistingSeat(
        Func<string, bool>? tryRaise,
        Func<string, bool>? isServed) : FfiFocusExistingSeat
    {
        public bool TryRaise(string actorId)
        {
            try { return tryRaise?.Invoke(actorId) ?? false; }
            catch (Exception) { return false; }
        }

        public bool IsServed(string actorId)
        {
            try { return isServed?.Invoke(actorId) ?? true; }
            catch (Exception) { return true; }
        }
    }
}
