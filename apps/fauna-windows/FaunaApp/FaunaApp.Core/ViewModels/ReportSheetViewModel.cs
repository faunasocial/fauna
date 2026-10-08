using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The shared report sheet's logic (<c>report-sheet</c> — moderation.md §
/// User-initiated reporting → <i>App surface</i>): ONE type for every surface that
/// opens it (feed ⋯, message ⋯, an OTHER profile), the windows twin of web's
/// <c>ReportHost.svelte</c> and tui's <c>report.rs</c>.
///
/// <para>Every decision here is shared Rust's (<c>fauna_client_moderation::report</c>
/// over UniFFI): the reason list, the submit gate, the include-text rule, the
/// words. The sheet paints <see cref="View"/> (<c>report_sheet_view</c>); the send
/// is <c>fauna.moderation.abuse_report.submit</c>, its request built from the sheet
/// by the shared <c>report_request</c>. What is left is the follow-up order, the
/// same one tui and web run: submit → <c>knocks_block(author)</c> when ticked →
/// the reporter-side hide (<c>hide_reported</c>) → the stored list into the render
/// state (<see cref="ContentPolicyCache.SetHiddenContent"/>).</para>
///
/// <para>A failed send keeps the sheet open for a retry and says so on the page's
/// <c>error-message</c> (<see cref="ReportOutcome.Error"/>); a failed block or hide
/// lands there too, BESIDE the acknowledgement (<see cref="ReportOutcome.FollowUpError"/>)
/// — never silent.</para>
/// </summary>
internal sealed class ReportSheetViewModel
{
    private readonly INestRpcClient _rpc;

    internal ReportSheetViewModel(INestRpcClient rpc, FfiReportTarget target)
    {
        _rpc = rpc;
        Target = target;
    }

    /// <summary>What is being reported, built by the shared <c>report_*_target</c>
    /// constructors — the sealed rule is carried there once.</summary>
    public FfiReportTarget Target { get; }

    /// <summary>The picked reason's wire token, or <c>null</c> until one is picked.</summary>
    public string? Reason { get; set; }

    public string Note { get; set; } = "";

    public bool IncludeText { get; set; }

    public bool BlockAuthor { get; set; }

    /// <summary>Whether a send is in flight (a second submit is refused).</summary>
    public bool Sending { get; private set; }

    /// <summary>The draft as the shared fold takes it.</summary>
    public FfiReportForm Form => new(Reason, Note, IncludeText, BlockAuthor);

    /// <summary>Everything the sheet paints, folded in shared Rust off the target
    /// and the draft (<c>report_sheet_view</c>).</summary>
    public FfiReportSheetView View => FaunaFfiMethods.ReportSheetView(Target, Form);

    /// <summary>The id the reporter-side hide keys on — a post's cid, a message's
    /// record cid, an account's actor id (the same key
    /// <c>content_render_for_item</c> matches).</summary>
    public string SubjectId => SubjectIdOf(Target.subject);

    /// <summary>The id a report subject names — a post's cid, a message's record cid,
    /// an account's actor id; empty for a subject kind a newer nest recorded that
    /// this build cannot read. Shared by the sheet's hide, the ledger line and the
    /// admin queue line, so the three can never name a subject differently.</summary>
    internal static string SubjectIdOf(FfiReportSubject subject) => subject switch
    {
        FfiReportSubject.Post p => p.cid,
        FfiReportSubject.Message m => m.recordCid,
        FfiReportSubject.Actor a => a.actorId,
        _ => "",
    };

    /// <summary>The subject's kind word as the admin queue line prints it
    /// (<c>post</c> / <c>message</c> / <c>actor</c>); <c>unknown</c> for a subject
    /// kind this build cannot read.</summary>
    internal static string SubjectKindOf(FfiReportSubject subject) => subject switch
    {
        FfiReportSubject.Post => "post",
        FfiReportSubject.Message => "message",
        FfiReportSubject.Actor => "actor",
        _ => "unknown",
    };

    /// <summary>Send the report and run the follow-ups. Never throws: every failure
    /// is a field of the returned <see cref="ReportOutcome"/>.</summary>
    public async Task<ReportOutcome> SubmitAsync()
    {
        if (Sending || !View.canSubmit) return ReportOutcome.NotSent;
        Sending = true;
        try
        {
            var form = Form;
            FfiReportSent reply;
            try
            {
                reply = await _rpc.AbuseReportSubmitAsync(Target, form);
            }
            catch (Exception ex)
            {
                // The sheet stays open for a retry.
                return ReportOutcome.Failed(
                    Strings.Resolve(FaunaFfiMethods.ReportFailed(Strings.Error(ex))));
            }

            string? followUp = null;
            string? blocked = null;
            if (form.blockAuthor && Target.author is { Length: > 0 } author)
            {
                try
                {
                    await _rpc.KnocksBlockAsync(author);
                    blocked = author;
                }
                catch (Exception ex)
                {
                    followUp = $"block: {Strings.Error(ex)}";
                }
            }

            try
            {
                ContentPolicyCache.SetHiddenContent(await _rpc.HideReportedAsync(SubjectId));
            }
            catch (Exception ex)
            {
                followUp ??= $"hide: {Strings.Error(ex)}";
            }

            return new ReportOutcome(
                Sent: true,
                Acknowledgement: Strings.Resolve(reply.acknowledgement),
                Error: null,
                FollowUpError: followUp,
                BlockedAuthor: blocked);
        }
        finally
        {
            Sending = false;
        }
    }
}

/// <summary>What a submit came to: the acknowledgement line when the report landed
/// (<c>report-status</c>), the failure that kept the sheet open when it did not, and
/// a failed block/hide follow-up that rides BESIDE a landed report.</summary>
internal sealed record ReportOutcome(
    bool Sent, string? Acknowledgement, string? Error, string? FollowUpError, string? BlockedAuthor)
{
    /// <summary>The submit gate refused (no reason, or a send already in flight).</summary>
    public static ReportOutcome NotSent { get; } = new(false, null, null, null, null);

    public static ReportOutcome Failed(string error) => new(false, null, error, null, null);
}
