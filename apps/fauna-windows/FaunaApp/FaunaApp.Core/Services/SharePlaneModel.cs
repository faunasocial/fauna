#if P2P_SHARE
using FaunaApp.Core.Logs;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The <b>peer-transfer plane's</b> observable surface on windows (<c>p2p.md</c> § Cross-user
/// shared-set transfer → <i>Implementation status today</i>, the FFI share-plane host paragraph;
/// <c>p2p-shared-set-build.md</c> § <i>Built — the shared driver</i>). The C# peer of macOS's
/// <c>SharePlaneModel</c> (priority #3: one concept, one shape).
///
/// <para><b>What lives here and what does not.</b> Everything app-agnostic — the driver, the
/// five seam answers, the durable advertisement sink, the six readings — is shared Rust behind
/// <c>FfiNestClient.StartSharePlane</c>. What is genuinely per-app is the three inputs the shell
/// hands over (the account's own sync-agent provisioner, a spool directory) and the repaint
/// nudge, which is this class.</para>
///
/// <para><b>No ordering contract with the caller.</b> <c>StartSharePlane</c> needs the account
/// runtime's store AND the conversations session, and both land on their own tasks after the
/// calls that start them return. Shared Rust waits for them (bounded, 180 s each), so
/// <see cref="StartAsync"/> is one call from wherever the provisioner starts — no retry loop, no
/// sequencing against <c>StartAccountRuntimeAsync</c> / <c>BuildConversationsSessionAsync</c>.</para>
///
/// <para>Excised with the <c>p2p-share</c> member (<c>P2P_SHARE</c>): the store-safe
/// <c>fauna_ffi.dll</c> exports no <c>FfiSharePlaneView</c>, <c>FfiSharePlaneListener</c> or
/// <c>StartSharePlane</c>.</para>
/// </summary>
internal sealed class SharePlaneModel
{
    /// <summary>Process-wide: the plane's state cell is itself a process global on the Rust side
    /// (<c>share_plane_view()</c>), one per session — this is its observable mirror, so the
    /// Folders page reads one instance however many times it is rebuilt.</summary>
    public static SharePlaneModel Shared { get; } = new();

    private readonly Func<FfiSharePlaneView?> _read;
    private FfiSharePlaneView? _view;

    public SharePlaneModel(Func<FfiSharePlaneView?>? read = null) =>
        _read = read ?? FaunaFfiMethods.SharePlaneView;

    /// <summary>The transfer surface's readings — <c>null</c> while the plane is not running on
    /// this device this session. <b>A different fact from "no shared folders to serve"</b>
    /// (a reading <i>inside</i> a non-null view): the surface renders nothing at all for
    /// <c>null</c>, because "this device is not running the plane" owes the user no line
    /// (<c>p2p.md</c> § <i>Three findings from the second leg</i>).</summary>
    public FfiSharePlaneView? View => Volatile.Read(ref _view);

    /// <summary>Raised after every re-read, from whichever thread did it (the plane's repaint
    /// nudges land on a Rust thread) — a subscriber marshals to the UI thread itself.</summary>
    public event Action? Changed;

    /// <summary>Re-read the plane's cell. Both repaint nudges land here, and so does sign-out
    /// (the account runtime's teardown forgets the cell, so this then reads <c>null</c> and the
    /// surface disappears with the session it belonged to).</summary>
    public void Refresh()
    {
        FfiSharePlaneView? next;
        try { next = _read(); }
        catch (Exception ex)
        {
            ShellLog.Warn("App", $"[share-plane] view read failed: {ex.Message}");
            next = null;
        }

        Volatile.Write(ref _view, next);
        try { Changed?.Invoke(); } catch { /* a repaint subscriber must never fault the Rust thread */ }
    }

    /// <summary>Start the plane for the signed-in account. Best-effort like every other
    /// post-auth hook: a failure is logged and leaves the surface absent, never failing the
    /// sign-in or the launch.</summary>
    /// <param name="rpc">This login's own live client — it holds the owner secret and the
    /// connected <c>FfiNestClient</c>.</param>
    /// <param name="provisioner">The app's own sync-agent provisioner — the plane's two agent
    /// verbs ride it.</param>
    /// <param name="spoolDir">App-private scratch for in-flight peer downloads.</param>
    public async Task StartAsync(INestRpcClient rpc, FfiSyncAgentProvisioner provisioner, string spoolDir)
    {
        try
        {
            Directory.CreateDirectory(spoolDir);
            await rpc.StartSharePlaneAsync(provisioner, spoolDir, new RepaintListener(this))
                .ConfigureAwait(false);
            Refresh();
        }
        catch (Exception ex)
        {
            ShellLog.Warn(
                "App",
                $"[share-plane] start failed; the peer-transfer surface stays absent this session: {ex.Message}");
        }
    }

    /// <summary>The plane's two repaint nudges. <b>Neither carries data</b> — each just tells the
    /// model to re-read <c>SharePlaneView()</c>, the one authoritative read, so
    /// <c>SeatBound</c> and <c>StateChanged</c> are deliberately the same act.</summary>
    private sealed class RepaintListener : FfiSharePlaneListener
    {
        private readonly SharePlaneModel _model;

        public RepaintListener(SharePlaneModel model) => _model = model;

        public void SeatBound() => _model.Refresh();

        public void StateChanged() => _model.Refresh();
    }
}

/// <summary>One <c>share-transfer-item</c>'s three readings, already rendered.</summary>
internal readonly record struct SharePlaneTransferReading(string Name, string Progress, string State);

/// <summary>
/// The six-id surface's readings.
///
/// <para>⚠ <b>Every reading here is resolved NESTED, never with <c>Strings.Resolve</c>.</b>
/// The transfer gate's honest refusal reads "Limited by {source}" where <c>{source}</c> is itself
/// an i18n KEY (<c>features.tier_admin</c>), not data — a plain substitution paints the raw key
/// at the user. tui's, linux's and macOS's readings resolve nested for the same reason and each
/// pins it with a unit test; so does this one (<c>SharePlaneReadingsTests</c>).</para>
/// </summary>
internal static class SharePlaneReadings
{
    /// <summary>The <c>share-serve-status</c> line.</summary>
    public static string ServeStatusText(FfiSharePlaneView view) =>
        Strings.ResolveNested(view.@serveStatus);

    /// <summary>The <c>share-transfer-list</c>'s rows, one per (set × peer) pull outcome.</summary>
    public static IReadOnlyList<SharePlaneTransferReading> TransferReadings(FfiSharePlaneView view) =>
        view.@transfers
            .Select(t => new SharePlaneTransferReading(
                Strings.ResolveNested(t.@name),
                Strings.ResolveNested(t.@progress),
                Strings.ResolveNested(t.@state)))
            .ToList();
}
#endif
