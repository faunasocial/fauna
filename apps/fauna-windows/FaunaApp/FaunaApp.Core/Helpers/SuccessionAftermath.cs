using System;
using System.Collections.Generic;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// windows' driver for the <b>post-succession aftermath</b>
/// (<c>docs/goal/behavior/succession-aftermath.md</c> § Re-key scope's
/// <c>BackupKey</c> corpus row — <i>"started at first successor sign-in,
/// surfaced with progress, resumed until complete"</i>). The windows twin of
/// apple's <c>FaunaKit/Core/SuccessionAftermath.swift</c>.
///
/// <para><b>What this fixes.</b> A succession re-points corpus <i>ownership</i>
/// on the nest, but every blob the predecessor sealed is still sealed under the
/// predecessor's <c>BackupKey</c>. Until the aftermath re-keys them, the
/// successor's own launch path reads its inherited corpus as <i>"wrong key or
/// tampered data"</i> — drafts come up empty, the MLS replica refuses to load
/// and the client stays single-device. The corpus is <b>stuck, not corrupt</b>:
/// the material to open it is in the account registry the whole time, and this
/// pass is what uses it.</para>
///
/// <para><b>This file composes; it does not sequence.</b> The order the legs run
/// in, and which of them are barriers, lives once in shared Rust
/// (<c>fauna_client_recovery::aftermath::run_succession_aftermath</c>, reached
/// here through the <c>RunSuccessionAftermath</c> FFI export). apple, tui and
/// web drive the same function. ⚠ Do not re-derive the ordering in C# — that is
/// exactly the shape the shared driver exists to remove.</para>
///
/// <para><b>Two legs deliberately run elsewhere on every app</b>, so their
/// absence here is the design: leg 3 (the <c>__mls</c> re-seal) is a barrier
/// inside <c>MlsStateSync::load</c>, and leg 5 (the file-corpus re-seal) is the
/// sync agent's. ⚠ Leg 3 is still handed an empty predecessor list on all four
/// FFI apps (<c>fauna-ffi</c>'s <c>mls_sync_launch.rs</c>) — that gap is
/// tracked separately and is shared by every FFI app, so it must be fixed once at
/// the FFI boundary and never forked into a C#-local workaround.</para>
///
/// <para>Same shape as <see cref="SealBackfillSweep"/> and
/// <see cref="CriticalAlertsSweep"/>: never throws, safe to fire unconditionally
/// from the post-auth hook without awaiting sign-in on it.</para>
/// </summary>
internal static class SuccessionAftermath
{
    /// <summary>
    /// Run the aftermath for the session that just authenticated.
    ///
    /// <para><b>Called on every authenticated start, not only after a
    /// ceremony</b> — the pass is resumable by design and returns
    /// <c>NotASuccessor</c> having done nothing for an identity that never
    /// succeeded, which is the overwhelmingly common case. Gating on "did we just
    /// succeed" would be wrong as well as unnecessary: a re-seal interrupted by a
    /// lost connection is finished by the <i>next</i> sign-in, and that sign-in
    /// has no ceremony to notice.</para>
    ///
    /// <para>Fire-and-forget and best-effort, in the shape
    /// <see cref="SealBackfillSweep"/> and <see cref="MailEpochSchedule"/> already
    /// use: the account is already the successor's, and refusing a session over a
    /// pass that retries would be strictly worse than a plane that is briefly
    /// still owed.</para>
    ///
    /// <para>The ceremony's raise context (the sweep's unattested-member roster and
    /// the nest's succession stamp) is parked durably in the account registry by
    /// shared Rust and drained by this pass, so no app carries it
    /// (<c>succession-aftermath.md</c> § Implementation status today).</para>
    /// </summary>
    internal static async Task RunAsync(INestRpcClient rpc)
    {
        // A new pass starts from nothing: every leg re-reports (null for one that
        // owes nothing), so a line from the PREVIOUS session's identity — an
        // account switch lands here too — must not survive into this one.
        AftermathProgress.Reset();
        InheritedFilterMarks.Reset();
        try
        {
            var outcome = await rpc
                .RunSuccessionAftermathAsync()
                .ConfigureAwait(false);
            // Logged unconditionally, including `NotASuccessor`. ⚠ The web
            // client's own history is the reason: a pass that reports a healthy
            // "nothing to do" is indistinguishable from one whose predecessor
            // LINK is missing, and that ambiguity cost a full 30-minute
            // diagnostic cycle there before the line was added.
            ShellLog.Info("SuccessionAftermath", $"succession aftermath: {outcome}");
        }
        catch (Exception ex)
        {
            ShellLog.Warn("SuccessionAftermath",
                $"run_succession_aftermath failed, best-effort: {ex.GetType().Name}: {ex.Message}");
        }
    }
}

/// <summary>
/// windows' <see cref="FfiAftermathSink"/>: each leg's already-localized line,
/// logged and recorded into <see cref="AftermathProgress"/>, which the Recovery
/// kit section renders (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The
/// post-succession aftermath's progress lines</i>; apple's
/// <c>LoggingAftermathSink</c> + <c>AftermathProgress</c> are the prior art). The
/// log line stays: it keeps a pass diagnosable from a real run's app log.
///
/// <para>⚠ <b>The status line comes from the shared projection, never from a C#
/// match on the leg.</b> Seven apps each mapping the progress enum themselves is
/// seven chances to say something different about one event (priorities #1/#3) —
/// which is why the FFI boundary hands over an already-resolved
/// <c>LocalizedText?</c> rather than the progress value.</para>
/// </summary>
internal sealed class LoggingAftermathSink : FfiAftermathSink
{
    // No stored state of its own: the shared state is AftermathProgress and
    // InheritedFilterMarks, which lock. Every method is called from the tokio
    // runtime the export runs on, never the UI thread; ShellLog is thread-safe.

    /// <summary>The session the pass runs over — what
    /// <see cref="ConfigStageSettled"/> reads the raised marks back through. Null
    /// only in unit tests that exercise the progress half alone.</summary>
    private readonly INestRpcClient? _rpc;

    internal LoggingAftermathSink(INestRpcClient? rpc = null) { _rpc = rpc; }

    public void Progress(FfiAftermathLeg leg, LocalizedText? line)
    {
        // `null` is a real value, not an absence: a leg whose outcome owes the
        // user nothing (`NothingConfigured`, `AlreadyEnrolled`, …) reports no
        // line at all, and the render hides it. Nothing to say is not an error
        // and must not read as one in the log either.
        // Recorded either way: a leg settling into "nothing to report" must
        // clear a stale line from an earlier report, not leave it painted.
        AftermathProgress.Record(leg, line);
        if (line is null)
        {
            ShellLog.Debug("SuccessionAftermath", $"aftermath {leg}: nothing to report");
            return;
        }
        ShellLog.Info("SuccessionAftermath", $"aftermath {leg}: {line.key}");
    }

    public void ConfigStageSettled()
    {
        // The config stage's raises have written: read the inherited-filter
        // marks back, so a successor's FIRST session shows the Account line and
        // the per-rule marks rather than waiting for the next sign-in (linux's
        // `config_stage_settled`, web's `configStageSettled`). Off this callback's
        // thread — the read goes back through the FFI, and re-entering the
        // runtime from inside its own callback is not a shape to lean on.
        // (The member-review surfaces re-read their roster on their own page
        // loads, so they need nothing from this hook.)
        if (_rpc is { } rpc) _ = Task.Run(() => InheritedFilterMarks.RefreshAsync(rpc));
    }
}

/// <summary>
/// The owner's open email-filter review marks — the ids of rules the
/// post-succession aftermath carried across and the owner has not yet answered
/// (<c>docs/goal/behavior/succession-aftermath.md</c> § Adjudicating what the
/// aftermath carries across). ONE cache feeding both surfaces that read it — the
/// Account section's <c>recovery-kit-inherited-filters-status</c> count and the
/// Email Filters list's per-rule <c>filter-unattested-mark</c> — so the two can
/// never disagree (web's module store, linux's <c>filter_marks</c> cell).
///
/// <para>Filled by <see cref="LoggingAftermathSink.ConfigStageSettled"/>, on every
/// Account visit and on every filter-list load. <b>A failed read leaves the cache
/// alone</b>: collapsing it into "nothing flagged" would hide a flagged rule.
/// <see cref="Changed"/> fires on whatever thread refreshed — subscribers
/// marshal.</para>
/// </summary>
internal static class InheritedFilterMarks
{
    private static readonly object Gate = new();
    private static HashSet<long> _ids = new();

    /// <summary>Raised after every successful refresh or reset.</summary>
    internal static event Action? Changed;

    internal static int Count
    {
        get { lock (Gate) return _ids.Count; }
    }

    internal static bool Contains(long filterId)
    {
        lock (Gate) return _ids.Contains(filterId);
    }

    /// <summary>Re-read the marks from the owner's succession ledger. Never throws.</summary>
    internal static async Task RefreshAsync(INestRpcClient rpc)
    {
        IReadOnlyList<long> ids;
        try
        {
            ids = await rpc.FilterMarksListAsync().ConfigureAwait(false);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("InheritedFilterMarks",
                $"filter_marks_list failed, keeping the cached marks: {ex.GetType().Name}: {ex.Message}");
            return;
        }
        lock (Gate) _ids = new HashSet<long>(ids);
        Changed?.Invoke();
    }

    /// <summary>Forget every mark — an account switch must not paint the
    /// previous identity's backlog.</summary>
    internal static void Reset()
    {
        lock (Gate) _ids = new HashSet<long>();
        Changed?.Invoke();
    }
}

/// <summary>
/// The post-succession aftermath's per-leg progress — windows' twin of apple's
/// <c>AftermathProgress</c>, web's module store and linux's struct
/// (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The post-succession
/// aftermath's progress lines</i>). Fed by <see cref="LoggingAftermathSink"/> as
/// each leg reports; read by the Settings page's Recovery kit section.
///
/// <para>Process-wide, because the sink is: the FFI calls it from its own runtime
/// and the page that reads it may not exist yet. <see cref="Changed"/> fires on
/// that runtime's thread — a subscriber marshals to its own UI thread.</para>
///
/// <para>A missing entry is a real value, not an absence: a leg whose outcome
/// owes the user nothing reports <c>null</c> from the shared projection, and the
/// render hides that line — never a C#-side decision (priorities #1/#3). Leg 5
/// (the file corpus) reports from the sync agent's own process into no FFI
/// app's sink, so it has no leg here at all.</para>
/// </summary>
internal static class AftermathProgress
{
    private static readonly object Gate = new();
    private static readonly Dictionary<FfiAftermathLeg, LocalizedText> Lines = new();

    /// <summary>Raised after every <see cref="Record"/> and <see cref="Reset"/>.</summary>
    internal static event Action? Changed;

    internal static void Record(FfiAftermathLeg leg, LocalizedText? line)
    {
        lock (Gate)
        {
            if (line is null) Lines.Remove(leg);
            else Lines[leg] = line;
        }
        Changed?.Invoke();
    }

    internal static LocalizedText? Line(FfiAftermathLeg leg)
    {
        lock (Gate)
        {
            return Lines.TryGetValue(leg, out var line) ? line : null;
        }
    }

    internal static void Reset()
    {
        lock (Gate)
        {
            Lines.Clear();
        }
        Changed?.Invoke();
    }
}
