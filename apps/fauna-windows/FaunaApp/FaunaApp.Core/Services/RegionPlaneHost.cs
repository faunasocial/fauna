using System;
using System.Collections.Generic;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;
using ContentLabelEntry = uniffi.fauna_core.ContentLabelEntry;

namespace FaunaApp.Core.Services;

/// <summary>
/// The region content plane on windows (<c>region-blocking.md</c> § The content
/// plane → <i>How an app obtains its region's policy</i>, <i>The blocked render and
/// the transparency surface</i>) — a paint of the shared <c>FfiRegionPlane</c>
/// (<c>libs/fauna-ffi/src/region.rs</c>), the one face the four UniFFI apps drive.
/// apple's <c>FaunaKit/Core/RegionStore.swift</c>, linux's
/// <c>apps/fauna-linux/src/region.rs</c> and web's <c>$lib/region.svelte.ts</c> are
/// the sibling legs.
///
/// <para>Everything that decides — verify, fold, persist, the refresh cadence, the
/// scorer join, the composed verdict, which verbs get a placeholder and which
/// language of the reason it shows — is shared Rust behind the plane. What is left
/// here is exactly what the design lets diverge: the leaf's answer (handed in by
/// the app shell, which alone can read the Windows region setting — see
/// <c>RegionLeaf</c> in the app assembly); where the device record lives
/// (<see cref="AccountStateDir.Base"/>, the install-scoped
/// <c>%LocalAppData%\Fauna</c> — never the <c>SecretStore</c>: an envelope may reach
/// 4 MiB and a Credential Manager blob caps at 2.5 KiB); and when the relay is
/// asked (login, every reconnect, and a one-minute <c>RefreshIfDue</c> tick on the
/// shared cadence).</para>
///
/// <para>Process-lifetime and kept across sign-out and identity switch: a region is
/// a fact about the device, not the account — so <see cref="ActorScope"/> calls
/// only <see cref="ClearSession"/>, which forgets the refresh clock.</para>
/// </summary>
public static class RegionPlaneHost
{
    /// The app's UI language — the authority's reason is shown in it where the
    /// authority wrote one (the app's own strings are English today; linux's
    /// <c>UI_LANG</c>, apple's <c>RegionStore.uiLang</c>).
    internal const string UiLang = "en";

    /// The one-minute tick; the shared cadence (<c>REFRESH_INTERVAL_SECS</c>,
    /// decided in Rust) says whether a tick asks.
    private static readonly TimeSpan TickInterval = TimeSpan.FromMinutes(1);

    private static readonly object _gate = new();
    private static FfiRegionPlane? _plane;
    private static Timer? _tick;
    private static INestRpcClient? _rpc;

    /// <summary>Raised (on whatever thread the fold landed on) whenever the plane's
    /// answer may have changed — a relay reply folded. Every surface that composed a
    /// verdict or painted the settings section re-reads.</summary>
    public static event Action? Changed;

    /// <summary>
    /// Open the plane — ahead of the first fetch — restoring the device record. The
    /// leaf's answer: <paramref name="declaredCode"/> (<c>null</c>: the platform
    /// reports no region) and its <paramref name="source"/>. Idempotent: the first
    /// call wins, because the leaf reads one OS setting per launch. In a
    /// test-capable build the shared e2e override replaces the code (keeping its
    /// source), so this needs no test seam of its own.
    /// </summary>
    internal static void Open(string? declaredCode, FfiRegionSource source)
    {
        lock (_gate)
        {
            if (_plane is not null) return;
            _plane = FfiRegionPlane.Open(declaredCode, source, AccountStateDir.Base);
        }
    }

    /// The plane, or <c>null</c> before the shell opened it (a unit test, a
    /// preview) — every reader then falls back to the family-only arms.
    internal static FfiRegionPlane? Plane
    {
        get { lock (_gate) return _plane; }
    }

    /// <summary>
    /// Ask the relay now (login / reconnect) and arm the minute tick, over the
    /// session's <paramref name="rpc"/> (its connected <c>FfiNestClient</c>).
    /// Best-effort: a failed ask writes nothing (§ Fail posture) and never throws
    /// into login.
    /// </summary>
    internal static async Task RefreshAsync(INestRpcClient rpc)
    {
        lock (_gate)
        {
            _rpc = rpc;
            _tick ??= new Timer(_ => _ = TickAsync(), null, TickInterval, TickInterval);
        }
        await AskAsync(onlyIfDue: false).ConfigureAwait(false);
    }

    private static Task TickAsync() => AskAsync(onlyIfDue: true);

    private static async Task AskAsync(bool onlyIfDue)
    {
        FfiRegionPlane? plane;
        INestRpcClient? rpc;
        lock (_gate)
        {
            plane = _plane;
            rpc = _rpc;
        }
        if (plane is null || rpc is null) return;
        try
        {
            if (await rpc.RefreshRegionPlaneAsync(plane, onlyIfDue).ConfigureAwait(false))
                Changed?.Invoke();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("RegionPlaneHost", $"region refresh skipped: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>Forget the refresh clock and the session's nest on an identity change,
    /// so the next login asks at once. The plane itself is the device's and stays.</summary>
    internal static void ClearSession()
    {
        FfiRegionPlane? plane;
        lock (_gate)
        {
            _rpc = null;
            plane = _plane;
        }
        plane?.ClearSession();
    }

    /// <summary>
    /// One item's render decision with the region composed in — the region, the
    /// guardian floor and the viewer's own thresholds, strictest-wins in shared Rust.
    /// With no plane open (a unit test, a preview) the family-only verdict stands and
    /// no region placeholder exists.
    /// </summary>
    internal static RegionRenderDecision Render(
        ContentLabelEntry[] labels, ContentPolicyInputs inputs, RegionSubject subject)
    {
        // The viewer's own reports are a third input beside the family floor and the
        // region (moderation.md § Corollary): ask the ITEM verdict, which carries the
        // hidden list, and fold it under the region's answer below.
        var item = inputs.ItemVerdictFor(labels, subject.ReportKey, subject.ReportAuthor);
        var plane = Plane;
        if (plane is null)
            return new RegionRenderDecision(item.Verdict, null, item.Reported);
        var r = plane.Render(
            labels, inputs.ContentPolicy, inputs.OwnSpamPermille, inputs.OwnPhishingPermille,
            subject.ContentIdHex, subject.AuthorHex, subject.Text, subject.Hashtags,
            subject.HasMedia, UiLang);
        var placeholder = RegionPlaceholderModel.From(r.@placeholder);
        if (!item.Reported)
            return new RegionRenderDecision(r.@verdict, placeholder, false);
        // Reported: the item is blocked whatever the region said. A region BLOCK keeps
        // its placeholder (convention 17: a region block never renders silent); a
        // region COLLAPSE is dropped — the viewer's own act is the more specific
        // explanation, and a reveal must not lift it.
        return new RegionRenderDecision(
            "block", placeholder is { IsBlock: true } ? placeholder : null, true);
    }

    /// <summary>What the settings region section paints; <c>null</c> before the plane
    /// is open.</summary>
    internal static FfiRegionView? View() => Plane?.View();

    // ── Convention 17: "a region Block never renders silent" ──────────────────
    //
    // `blocked` is the set of on-screen items whose composed verdict the region
    // blocks, registered by each surface's item container from the verdict it
    // computed off its snapshot (independent of which arm painted); `painted` is the
    // set of block placeholders actually realized. An arm that drops the placeholder
    // shows up as `placeholders < blocked`. apple's RegionBlockRender, tui's and
    // linux's `region::block_render_json`. Keys name the surface + item. UI-thread
    // only (a container's Loaded/Unloaded/DataContextChanged), like the reveal set.
    private static readonly HashSet<string> _blocked = new();
    private static readonly HashSet<string> _painted = new();

    /// Register (or clear) an on-screen item container's verdict-side walk.
    public static void WitnessBlocked(string key, bool blocked)
    {
        if (blocked) _blocked.Add(key); else _blocked.Remove(key);
    }

    /// Register (or clear) a realized region BLOCK placeholder.
    public static void WitnessPainted(string key, bool painted)
    {
        if (painted) _painted.Add(key); else _painted.Remove(key);
    }

    /// Drop an item container's registrations when it leaves the screen.
    public static void Forget(string key)
    {
        _blocked.Remove(key);
        _painted.Remove(key);
    }

    /// The <c>region_block_render</c> state field
    /// (<c>tests/e2e-unified/helpers/frame_invariants.py</c>).
    public static Dictionary<string, object?> BlockRenderState() => new()
    {
        ["blocked"] = _blocked.Count,
        ["placeholders"] = _painted.Count,
    };
}

/// <summary>The scorer input a bundled region scorer reads for one item — the item's
/// id, author, text, hashtags and whether it carries media
/// (<c>FfiRegionPlane.render</c>). apple's <c>RegionSubject</c>, linux's
/// <c>region::post_input</c> / <c>message_input</c>.</summary>
internal sealed record RegionSubject(
    string? ContentIdHex, string? AuthorHex, string Text, string[] Hashtags, bool HasMedia,
    string? ReportKey = null, string? ReportAuthor = null)
{
    /// A feed post (card or detail). Its report key is the post's cid + author.
    public static RegionSubject Post(uniffi.fauna_feed.PostSummary p) =>
        new(p.postId, p.author, p.body, p.tags, p.hasMedia, p.postId, p.author);

    /// A conversation bubble, post-decrypt — the zero id stands in for the author,
    /// as on tui, linux and apple. <paramref name="reportKey"/> is the message's
    /// plane record DIGEST (never its message id — the id a report names is the
    /// digest, <c>report_message_subject</c>) and <paramref name="reportAuthor"/>
    /// the sender's actor id; both null for a message with no plane ref (mail,
    /// bridged), which cannot be reported or hidden.
    public static RegionSubject Message(
        string id, string text, string? reportKey = null, string? reportAuthor = null) =>
        new(id, null, text, Array.Empty<string>(), false, reportKey, reportAuthor);
}

/// <summary>A region placeholder, ready to paint: the verb, the region, the
/// authority's name, its reason verbatim, and the app's frame naming the region and
/// its authority (<c>region.blocked_notice</c> / <c>region.collapsed_notice</c>). A
/// public record (the generated <c>FfiRegionPlaceholder</c> is UniFFI-internal) so
/// XAML can bind it.</summary>
public sealed record RegionPlaceholderModel(
    string Verb, string Region, string AuthorityName, string Reason)
{
    /// Whether this placeholder is a <c>block</c> (no reveal) rather than a
    /// <c>collapse</c> (one reveal away, sharing the family reveal set).
    public bool IsBlock => Verb == "block";

    /// The app's frame — the region and its authority, in the verb's words.
    public string NoticeText => Strings.Resolve(new uniffi.fauna_core.LocalizedText(
        IsBlock ? "region.blocked_notice" : "region.collapsed_notice",
        new Dictionary<string, string> { ["region"] = Region, ["authority"] = AuthorityName }));

    internal static RegionPlaceholderModel? From(FfiRegionPlaceholder? p) =>
        p is null ? null : new(p.@verb, p.@region, p.@authorityName, p.@reason);
}

/// <summary>One item's render decision: the composed verdict, and the region
/// placeholder to paint AHEAD of the family arm when the region drove a
/// <c>block</c> or <c>collapse</c> (<c>null</c> → the app's existing arms).</summary>
internal sealed record RegionRenderDecision(
    string Verdict, RegionPlaceholderModel? Placeholder, bool Reported = false)
{
    /// Whether the region blocks this item — convention 17's verdict side.
    public bool IsRegionBlocked => Placeholder?.IsBlock == true;
}
