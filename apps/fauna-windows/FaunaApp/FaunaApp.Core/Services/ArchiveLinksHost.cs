using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using uniffi.fauna_archive_import_machine;

namespace FaunaApp.Core.Services;

/// <summary>
/// Phase two of archive import on windows — the account's archive links
/// (<c>docs/goal/behavior/archive-import.md</c> § Linking and merging, ruling 8).
///
/// <para>Everything that decides is shared Rust: the links are the machine crate's
/// <c>ArchiveLinks</c>, which the FFI factory builds with the conversations session and
/// registers as its archive-signal sink (<c>FfiNestClient.ArchiveLinks()</c>, read through
/// <see cref="INestRpcClient.ArchiveLinksAsync"/>); each contact's state is its
/// <see cref="ArchiveLinkView"/> and the row's words the crate's <c>text</c> composers.
/// This host only drives and caches — tui's <c>archive_glue</c> /
/// <c>archive_links_tick</c> shape, the twin of FaunaKit's <c>ArchiveLinksStore</c> and
/// android's <c>ArchiveLinksHost</c>:</para>
/// <list type="bullet">
/// <item><see cref="Start"/> — from the login path once the conversations session is
/// built: one <c>DriveLinks()</c> now, then a one-second tick that drives again whenever
/// <c>LinkWorkDue()</c> reads true (the sink never sends — it runs inside the receive
/// loop) and re-reads the views whenever <c>LinkCaptures()</c> moved;</item>
/// <item><see cref="RefreshAsync"/> — the Contacts page re-reads the views with its
/// roster;</item>
/// <item><see cref="AnswerAsync"/> / <see cref="UnlinkAsync"/> — the row's
/// <c>contact-archive-link-yes</c> / <c>-no</c> / <c>contact-archive-unlink</c>;</item>
/// <item><see cref="Current"/> — the archive-import page attaches the links to its
/// machine before every run (<c>AttachLinks</c>), so a finished run announces itself.</item>
/// </list>
///
/// <para>Stopped by <see cref="ActorScope.DropActorScopedState"/>, so an account switch
/// never paints the outgoing account's links. <see cref="Changed"/> fires on whatever
/// thread finished the read; a page marshals it onto its own dispatcher.</para>
/// </summary>
internal sealed class ArchiveLinksHost
{
    /// <summary>The one host of the process — the account's links live for one login.</summary>
    public static ArchiveLinksHost Instance { get; } = new(TimeSpan.FromSeconds(1));

    /// <summary>The tick's cadence (tui's <c>archive_links_tick</c>); <c>null</c> runs no
    /// timer — a unit test drives <see cref="TickAsync"/> itself.</summary>
    private readonly TimeSpan? _tickInterval;

    private readonly object _gate = new();
    private IArchiveLinks? _links;
    /// <summary>The <c>LinkCaptures()</c> count the views were last re-read at.</summary>
    private ulong _seenCaptures;
    private IReadOnlyList<ArchiveLinkView> _views = Array.Empty<ArchiveLinkView>();
    private ulong _revision;
    /// <summary>Refresh tickets, so a slower read that started first never overwrites the
    /// answer of a later one (a drive's refresh can overlap the tick's).</summary>
    private ulong _refreshIssued;
    private ulong _refreshApplied;
    /// <summary>Single-flight guard: one <c>DriveLinks()</c> at a time (tui's
    /// <c>archive_links_drive</c> flag).</summary>
    private bool _driving;
    private CancellationTokenSource? _tick;

    internal ArchiveLinksHost(TimeSpan? tickInterval) => _tickInterval = tickInterval;

    /// <summary>The views moved (a read answered something new, or a stop dropped
    /// them). Raised off the UI thread.</summary>
    public event Action? Changed;

    /// <summary>The links the session built, for the archive-import machine's
    /// <c>AttachLinks</c> before a run; <c>null</c> while none are wired.</summary>
    public IArchiveLinks? Current { get { lock (_gate) return _links; } }

    /// <summary>The latest read of every pair's view; empty before the first read and
    /// for a login that wired no links.</summary>
    public IReadOnlyList<ArchiveLinkView> Views { get { lock (_gate) return _views; } }

    /// <summary>Bumped whenever <see cref="Views"/> changes, so a page repaints only when
    /// it moved (the <c>ContactOverlayNames</c> revision shape).</summary>
    public ulong Revision { get { lock (_gate) return _revision; } }

    /// <summary>The drive the last <see cref="Start"/> or <see cref="TickAsync"/> spawned —
    /// a unit test awaits it; production never does.</summary>
    internal Task LastDrive { get; private set; } = Task.CompletedTask;

    /// <summary>Adopt <paramref name="fresh"/> and start driving it; <c>null</c> (a
    /// session with no sync device id, or an FFI flavor without archive import) stops.
    /// Idempotent for the same links.</summary>
    public void Start(IArchiveLinks? fresh)
    {
        if (fresh is null)
        {
            Stop();
            return;
        }
        bool dropped;
        CancellationToken? tick = null;
        lock (_gate)
        {
            if (ReferenceEquals(_links, fresh)) return;
            dropped = StopLocked();
            _links = fresh;
            _seenCaptures = fresh.LinkCaptures();
            if (_tickInterval is not null)
            {
                _tick = new CancellationTokenSource();
                // Read under the lock: a racing Stop disposes the source.
                tick = _tick.Token;
            }
        }
        if (dropped) Changed?.Invoke();
        if (tick is { } ct) _ = RunTickAsync(_tickInterval!.Value, ct);
        Drive(fresh);
    }

    /// <summary>Stop the tick and drop the links and their views.</summary>
    public void Stop()
    {
        bool dropped;
        lock (_gate) dropped = StopLocked();
        if (dropped) Changed?.Invoke();
    }

    /// <summary>Answers whether views were dropped (so the caller raises
    /// <see cref="Changed"/> outside the lock).</summary>
    private bool StopLocked()
    {
        _tick?.Cancel();
        _tick?.Dispose();
        _tick = null;
        _links = null;
        _seenCaptures = 0;
        _driving = false;
        // A read already in flight must not land after the stop.
        _refreshApplied = _refreshIssued;
        if (_views.Count == 0) return false;
        _views = Array.Empty<ArchiveLinkView>();
        _revision++;
        return true;
    }

    /// <summary>Re-read every contact's link view. Best-effort: a failed read keeps the
    /// previous views rather than blanking a live prompt.</summary>
    public async Task RefreshAsync()
    {
        IArchiveLinks? links;
        ulong ticket;
        lock (_gate)
        {
            links = _links;
            if (links is null) return;
            ticket = ++_refreshIssued;
        }
        ArchiveLinkView[] fresh;
        try
        {
            fresh = await links.LinkViews().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("ArchiveLinks", $"views: {ex.Message}");
            return;
        }
        bool moved;
        lock (_gate)
        {
            // A stop, a re-point or a later read owns the views now.
            if (!ReferenceEquals(_links, links) || ticket <= _refreshApplied) return;
            _refreshApplied = ticket;
            moved = !_views.SequenceEqual(fresh);
            if (moved)
            {
                _views = fresh;
                _revision++;
            }
        }
        if (moved) Changed?.Invoke();
    }

    /// <summary>The row's same-person answer — once, either way.</summary>
    public async Task AnswerAsync(string contact, string platform, bool yes)
    {
        var links = Current;
        if (links is null) return;
        try
        {
            await links.AnswerLinkToken(contact, platform, yes).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("ArchiveLinks", $"answer: {ex.Message}");
        }
        await RefreshAsync().ConfigureAwait(false);
    }

    /// <summary>Unlink a linked contact: future merges stop, claimed content stays.</summary>
    public async Task UnlinkAsync(string contact, string platform)
    {
        var links = Current;
        if (links is null) return;
        try
        {
            await links.UnlinkToken(contact, platform).ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("ArchiveLinks", $"unlink: {ex.Message}");
        }
        await RefreshAsync().ConfigureAwait(false);
    }

    /// <summary>One tick: drive when work is due (spawned, single-flight), re-read the
    /// views when the sink captured something since the last look. Both checks are
    /// atomic loads in shared Rust, so the folders are read only when one fires.</summary>
    internal async Task TickAsync()
    {
        var links = Current;
        if (links is null) return;
        if (links.LinkWorkDue()) Drive(links);
        var captures = links.LinkCaptures();
        bool moved;
        lock (_gate)
        {
            if (!ReferenceEquals(_links, links)) return;
            moved = captures != _seenCaptures;
            _seenCaptures = captures;
        }
        if (moved) await RefreshAsync().ConfigureAwait(false);
    }

    private async Task RunTickAsync(TimeSpan every, CancellationToken ct)
    {
        try
        {
            using var timer = new PeriodicTimer(every);
            while (await timer.WaitForNextTickAsync(ct).ConfigureAwait(false))
            {
                try { await TickAsync().ConfigureAwait(false); }
                catch (Exception ex) { ShellLog.Warn("ArchiveLinks", $"tick: {ex.Message}"); }
            }
        }
        catch (OperationCanceledException) { /* stopped */ }
    }

    private void Drive(IArchiveLinks links)
    {
        lock (_gate)
        {
            if (_driving || !ReferenceEquals(_links, links)) return;
            _driving = true;
        }
        LastDrive = DriveAsync(links);
    }

    private async Task DriveAsync(IArchiveLinks links)
    {
        try
        {
            await links.DriveLinks().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("ArchiveLinks", $"drive: {ex.Message}");
        }
        finally
        {
            lock (_gate)
            {
                if (ReferenceEquals(_links, links)) _driving = false;
            }
        }
        await RefreshAsync().ConfigureAwait(false);
    }

    /// <summary>The row paints for the roster row of <paramref name="actorId"/>, whose
    /// shown name is <paramref name="contactName"/>: the shared views filtered to that
    /// contact (hex, case-blind), worded by the machine crate's composers
    /// (<c>archive_link_prompt</c> / <c>archive_linked_label</c>) — the FaunaKit
    /// <c>archiveLinkRowPaints</c> / android <c>archiveLinkRowPaints</c> twin. Only an
    /// asked pair asks and only a linked one offers unlink; offered, declined and
    /// unlinked pairs paint nothing.</summary>
    internal static IReadOnlyList<ArchiveLinkRowPaint> RowPaints(
        IReadOnlyList<ArchiveLinkView> views, string actorId, string contactName)
    {
        List<ArchiveLinkRowPaint>? paints = null;
        foreach (var view in views)
        {
            if (!string.Equals(view.contact, actorId, StringComparison.OrdinalIgnoreCase)) continue;
            ArchiveLinkRowPaint? paint = view.state switch
            {
                ArchiveLinkState.Asked => new ArchiveLinkRowPaint(
                    ArchiveLinkRowKind.Asked,
                    Strings.Resolve(FaunaArchiveImportMachineMethods.ArchiveLinkPrompt(view, contactName)),
                    view.contact, view.platform),
                ArchiveLinkState.Linked => new ArchiveLinkRowPaint(
                    ArchiveLinkRowKind.Linked,
                    Strings.Resolve(FaunaArchiveImportMachineMethods.ArchiveLinkedLabel(view)),
                    view.contact, view.platform),
                _ => null,
            };
            if (paint is not null) (paints ??= new()).Add(paint);
        }
        return paints ?? (IReadOnlyList<ArchiveLinkRowPaint>)Array.Empty<ArchiveLinkRowPaint>();
    }
}
