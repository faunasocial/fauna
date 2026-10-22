using System;
using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Helpers;
using FaunaApp.Core.Services;
using uniffi.fauna_client_moderation;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Drives the standalone Moderation page: the moderation queue + per-item training
/// correction, over <see cref="INestRpcClient"/>. The queue is the <b>union</b> of the
/// server <c>fauna.moderation.actions</c> obligation rows (the shared
/// <c>FfiModerationClient</c> seam linux/apple consume) and the client's own
/// post-decrypt <b>local detections</b> from the conversations session
/// (<see cref="IModerationLocalDetections"/>), merged through the one shared
/// <c>fauna_client_moderation::merge_queue</c> façade (moderation.md § Layout &amp;
/// flow). A local row's <c>train-correction-button</c> removes the client-side flag; a
/// server row's runs the one shared correction flow
/// (<c>MailSettingsMachine.TrainModerationCorrection</c>).
///
/// <para>The spam-filtering <b>preferences</b> (<c>fauna.spam.{get,set}_preferences</c>)
/// live on Settings → Privacy (the canonical <c>spam-moderation-controls</c> home,
/// <c>SettingsViewModel</c> + <c>SettingsPrivacyPage</c>), not here — this page is
/// queue-only, matching linux/macOS/iOS/Android and ui.yaml's <c>moderation</c> scope.</para>
/// </summary>
public partial class ModerationViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;

    // Moderation queue — the UNION of the caller's own server-issued obligation rows
    // (fauna.moderation.actions) and the client's post-decrypt local detections
    // (moderation.md § Layout & flow); each row carries a train-correction.
    public ObservableCollection<ModerationRow> Actions { get; } = new();

    // The reporter's own ledger (`moderation-reports-section`; moderation.md §
    // User-initiated reporting → What the reporter is told), read beside the queue on
    // every Moderation entry. Every word is shared Rust's: the row's reason / status /
    // outcome / destinations arrive worded, the title and the empty line are
    // report_ledger_title / report_ledger_empty, a withdraw's line is
    // report_withdraw_verdict.
    public ObservableCollection<ReportLedgerRowView> Reports { get; } = new();

    /// <summary>Whether a ledger read has landed. The empty line paints only off this
    /// bit, so a read in flight never reads as "you have not reported anything"
    /// (ui/README.md § List pages: loading is not empty).</summary>
    [ObservableProperty] private bool _reportsLoaded;

    // ── Appeal (moderation.md § Legal takedown — the transparency triple's second
    //     leg). Every decision is pre-made in shared Rust: `row_is_appealable` picks
    //     the rows that offer a handle, `appeal_form_view` owns the guard and the
    //     wording, `appeal_verdict` the outcome sentence. This VM paints that fold and
    //     dispatches; it decides and words nothing. Mirrors linux's `AppealDraft` /
    //     `finish_appeal` and android's `ModerationQueueVM.openAppeal`/`submitAppeal`.

    /// The decision being appealed, captured when the form OPENED. The queue
    /// refetches under an open form, so a dispatch keyed on the row's index could
    /// appeal the wrong decision.
    private string? _appealContentId;

    /// <summary>Whether the appeal form is open (<c>appeal-reason-input</c>,
    /// <c>appeal-submit-button</c> and <c>appeal-cancel-button</c> paint only
    /// while true — absent, not merely disabled).</summary>
    [ObservableProperty] private bool _appealOpen;

    /// <summary><c>appeal-reason-input</c> — the reason as typed.</summary>
    [ObservableProperty] private string _appealReason = string.Empty;

    /// <summary>Whether <c>appeal-submit-button</c> may be offered — the shared
    /// fold's <c>can_submit</c> verbatim.</summary>
    [ObservableProperty] private bool _appealCanSubmit;

    /// <summary>Why the submit control is disabled, or <c>null</c> exactly when
    /// <see cref="AppealCanSubmit"/> is true.</summary>
    [ObservableProperty] private string? _appealBlockedReason;

    /// <summary><c>appeal-submit-button</c>'s label (the fold's <c>submit_label</c>).</summary>
    [ObservableProperty] private string _appealSubmitLabel = string.Empty;

    /// <summary>Names the content the appeal is filed against (the fold's <c>summary</c>).</summary>
    [ObservableProperty] private string _appealSummary = string.Empty;

    /// <summary><c>appeal-status</c> — the last attempt's outcome. Page-level so it
    /// survives the form closing on success, and deliberately NOT
    /// <see cref="ViewModelBase.ErrorMessage"/>: a refused appeal is form feedback
    /// that must not blank the queue the user is reading. <c>null</c> until an
    /// attempt (and again after a cancel).</summary>
    [ObservableProperty] private string? _appealStatus;

    private bool _appealBusy;

    partial void OnAppealReasonChanged(string value) => RefoldAppeal();

    private void RefoldAppeal()
    {
        var view = FaunaFfiMethods.AppealFormView(_appealContentId ?? string.Empty, AppealReason ?? string.Empty);
        AppealCanSubmit = view.canSubmit;
        AppealBlockedReason = view.blockedReason is { } reason ? Strings.Resolve(reason) : null;
        AppealSubmitLabel = Strings.Resolve(view.submitLabel);
        AppealSummary = Strings.Resolve(view.summary);
    }

    /// <summary><c>appeal-button</c>: open the form on <paramref name="row"/>,
    /// capturing its content id now.</summary>
    public void OpenAppeal(ModerationRow row)
    {
        _appealContentId = row.ContentId;
        AppealStatus = null;
        AppealReason = string.Empty;
        RefoldAppeal();   // the reason may already have been empty (no change event)
        AppealOpen = true;
    }

    /// <summary><c>appeal-cancel-button</c>: drop the draft and the stale status
    /// together, touching nothing nest-side.</summary>
    public void CancelAppeal()
    {
        _appealContentId = null;
        AppealOpen = false;
        AppealReason = string.Empty;
        AppealStatus = null;
    }

    /// <summary><c>appeal-submit-button</c>: dispatch the captured draft. Re-gates on
    /// the shared <c>can_submit</c> rather than trusting the control's enabled state.
    /// There is no offline gate: <c>fauna.moderation.appeal</c> is
    /// <c>OfflineQueued</c>. Success CLOSES the form (the decision is with an
    /// administrator now; the row stays — appeals are additive history); failure
    /// KEEPS it open with the reason intact so it can be fixed and resent.</summary>
    public async Task SubmitAppealAsync()
    {
        if (_appealBusy || !AppealOpen || _appealContentId is not { } contentId) return;
        var reason = AppealReason ?? string.Empty;
        if (!FaunaFfiMethods.AppealFormView(contentId, reason).canSubmit) return;

        _appealBusy = true;
        string? error = null;
        try
        {
            await _rpc.ModerationAppealAsync(contentId, reason);
        }
        catch (Exception ex)
        {
            error = Strings.Error(ex);
        }
        finally
        {
            _appealBusy = false;
        }
        AppealStatus = Strings.Resolve(FaunaFfiMethods.AppealVerdict(error));
        if (error is null)
        {
            _appealContentId = null;
            AppealOpen = false;
            AppealReason = string.Empty;
        }
    }

    /// <summary>The line the last withdraw painted (its verdict), or <c>null</c>.</summary>
    [ObservableProperty] private string? _reportWithdrawStatus;

    /// <summary>The ledger's header (<c>report_ledger_title</c>).</summary>
    public string ReportsTitle => Strings.Resolve(FaunaFfiMethods.ReportLedgerTitle());

    /// <summary>The ledger's empty line (<c>report_ledger_empty</c>).</summary>
    public string ReportsEmptyText => Strings.Resolve(FaunaFfiMethods.ReportLedgerEmpty());

    private readonly INestRpcClient _rpc;
    // The client half of the queue: the conversations session's post-decrypt local
    // detections. Null when there is no MLS session (→ the server rows alone).
    private readonly IModerationLocalDetections? _local;
    // The shared-Rust correction seam (the sealed spam-model write + the always-run
    // fauna.moderation.train; mail-spam.md § Encrypted-mode interaction). Always supplied
    // in production; null only in queue-read tests, where a server-row correction fails.
    private readonly ISpamModelClientWrite? _spamWrite;

    internal ModerationViewModel(
        INestRpcClient rpc,
        IModerationLocalDetections? local = null,
        ISpamModelClientWrite? spamWrite = null)
    {
        _rpc = rpc;
        _local = local;
        _spamWrite = spamWrite;
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            // Moderation queue (fauna.moderation.actions) over the shared
            // FfiModerationClient seam. An empty queue is normal (nothing flagged);
            // a genuine read error surfaces via the outer catch.
            await RefreshQueueAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsLoading = false;
        }
        // The ledger rides the same entry, but its own read: an unavailable ledger
        // must never cost the queue, nor paint an empty line it cannot vouch for.
        await RefreshReportsAsync();
    }

    /// <summary>Re-read the reporter's ledger (<c>fauna.moderation.abuse_report.mine</c>).
    /// A failed read leaves <see cref="ReportsLoaded"/> as it was — never an empty
    /// line off a read that did not land.</summary>
    private async Task RefreshReportsAsync()
    {
        try
        {
            var rows = await _rpc.AbuseReportMineAsync();
            Reports.Clear();
            foreach (var r in rows)
                Reports.Add(MapLedgerRow(r));
            ReportsLoaded = true;
        }
        catch (Exception ex)
        {
            Logs.ShellLog.Warn("ModerationViewModel", $"report ledger read skipped: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>Withdraw one of the caller's open reports
    /// (<c>fauna.moderation.abuse_report.withdraw</c>), say how it went in the shared
    /// verdict's words, and re-read the ledger — the row then reads withdrawn and
    /// offers no second withdraw.</summary>
    [RelayCommand]
    private async Task WithdrawReportAsync(ReportLedgerRowView row)
    {
        string? failure = null;
        try
        {
            await _rpc.AbuseReportWithdrawAsync(row.ReportId);
        }
        catch (Exception ex)
        {
            failure = Strings.Error(ex);
        }
        ReportWithdrawStatus = Strings.Resolve(FaunaFfiMethods.ReportWithdrawVerdict(failure));
        await RefreshReportsAsync();
    }

    /// <summary>One ledger row's line — <c>reason · status · subject · outcome — destination(s)</c>,
    /// every part already worded by shared Rust; only the join is here, the same one
    /// web's <c>ledgerLine</c> and tui's <c>ledger_elements</c> make (the e2e reads it).</summary>
    internal static ReportLedgerRowView MapLedgerRow(FfiReportLedgerRow r)
    {
        var parts = new List<string>
        {
            Strings.Resolve(r.reason),
            Strings.Resolve(r.status),
            FaunaFfiMethods.ShortId(ReportSheetViewModel.SubjectIdOf(r.subject)),
        };
        if (r.outcome is { } outcome) parts.Add(Strings.Resolve(outcome));
        return new ReportLedgerRowView(
            r.reportId,
            $"{string.Join(" · ", parts)} — {Strings.Resolve(r.routedTo)}",
            r.canWithdraw);
    }

    [RelayCommand]
    private async Task TrainAsync(ModerationRow action)
    {
        try
        {
            if (action.IsLocal)
            {
                // A local detection has no server obligation to train against. Read the
                // retained decrypted body BEFORE dropping the flag (mirroring linux
                // correct_moderation_row order), train the caller's Bayesian model "not
                // spam" client-side over that text, then remove the false-positive flag
                // (moderation.md § State: local detections are held client-side) and
                // repaint. The machine self-gates: a nest without spam-model-sealed-at-rest
                // yields ServerPath → a silent no-op (there is no server train path for
                // local content). A null body (aged out) ⇒ just remove.
                var text = _local?.Body(action.ContentId);
                _local?.Remove(action.ContentId);
                if (_spamWrite is not null && !string.IsNullOrEmpty(text))
                    await _spamWrite.TrainSpamModelClientAsync(text, isSpam: false);
            }
            else
            {
                // A server row lists the caller's OWN flagged content, so the correction
                // is "not spam" — it down-weights the false positive in the caller's
                // Bayesian model. One shared flow owns both halves (the sealed model write
                // when the nest allows it, then fauna.moderation.train ALWAYS for the read
                // gate + report capture; moderation.md § Training correction submission):
                // MailSettingsMachine.train_moderation_correction.
                if (_spamWrite is null)
                    throw new InvalidOperationException("no mail settings machine to run the training correction");
                await _spamWrite.TrainModerationCorrectionAsync(action.ContentId, isSpam: false);
            }
            // Reflect the correction: re-read the queue.
            await RefreshQueueAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    private async Task RefreshQueueAsync()
    {
        // The server half: the caller's own obligation rows (fauna.moderation.actions).
        var server = await _rpc.ModerationActionsAsync();
        // The client half: the conversations session's post-decrypt local detections
        // (empty when there is no MLS session — the server rows alone, matching linux's
        // active_session() → None). The shared classify writer already populated the
        // store on decrypt, so reading it is all this leg does (no per-app writer).
        var local = _local?.Snapshot() ?? Array.Empty<LocalDetection>();
        // Merge through the ONE shared union (dedupe by content_id, server row winning,
        // newest-first) so the rule never drifts per client (moderation.md § Layout &
        // flow; fauna_client_moderation::merge_queue via the UniFFI façade).
        var rows = FaunaFfiMethods.ModerationQueue(server.ToArray(), local.ToArray());
        Actions.Clear();
        foreach (var r in rows)
            Actions.Add(MapRow(r));
    }

    /// <summary>Map a shared <c>QueueRow</c> (server obligation ∪ local detection) to a
    /// display row, resolving the badge (label/icon/accent) from the shared
    /// <c>content_label_style</c> map — no per-app category strings or colours
    /// (moderation.md § Categories &amp; enforcement). A <b>server</b> row renders its
    /// enforcement action via <c>obligation_action_label</c>; a <b>local</b> detection
    /// (<c>action == null</c>) has a <b>blank</b> action column — never a fabricated
    /// action (§ Don't do these).</summary>
    private static ModerationRow MapRow(QueueRow r)
    {
        var style = FaunaFfiMethods.ContentLabelStyle(r.category);
        string? actionLabel = r.action is byte action
            ? Strings.Resolve(FaunaFfiMethods.ObligationActionLabel(action))
            : null;
        // per-mille (0–1000) → whole percent, rounded half-up, single-sourced via
        // the shared fauna_core::format::confidence_percent (no per-app
        // (m + 5) / 10 — value-formatting.md § Confidence percent).
        var confidencePercent = (int)FaunaFfiMethods.ConfidencePercent(r.confidencePerMille);
        return new ModerationRow(
            ContentId: r.contentId,
            ContentType: r.contentType,
            CategoryLabel: Strings.Resolve(style.label),
            CategoryIcon: style.icon,
            CategoryAccent: style.accent,
            ActionLabel: actionLabel ?? string.Empty,
            ConfidencePercent: confidencePercent,
            IsLocal: r.source == QueueRowSource.Local,
            BadgeAttributes: BadgeAttributes(r.category, confidencePercent, actionLabel),
            // The shared rule: only a nest-issued decision has anything to appeal.
            IsAppealable: FaunaFfiMethods.RowIsAppealable(r));
    }

    /// <summary>The <c>content-label-badge</c>'s automation attributes (moderation.md
    /// § Element IDs → <i>Queue row facts</i>): <c>confidence</c>, the whole percent with
    /// its <c>%</c> exactly as the row paints it, and <c>action</c>, the resolved action
    /// label — absent on a local detection, which carries none. UIA gives the badge one
    /// HelpText, so they ride one <see cref="AutomationAttributePack"/>; the shared
    /// category key the badge styles from rides with them, so a local row's pack still
    /// holds two pairs. A lone pair would read back as a bare value
    /// (<c>flaui-bridge/PackedAttr.cs</c>), answering <c>action</c> with the whole
    /// <c>confidence=…</c> pair instead of nothing.</summary>
    internal static string BadgeAttributes(string category, int confidencePercent, string? actionLabel)
    {
        var confidence = ("confidence", $"{confidencePercent}%");
        return actionLabel is null
            ? AutomationAttributePack.Pack(("category", category), confidence)
            : AutomationAttributePack.Pack(("category", category), confidence, ("action", actionLabel));
    }
}

/// <summary>
/// One row of the reporter's own ledger (<c>moderation-report-item</c>): the id the
/// withdraw acts on, the shared-worded line the e2e reads, and whether the row is
/// still open (<c>moderation-report-withdraw-button</c> paints only then).
/// </summary>
public record ReportLedgerRowView(string ReportId, string Line, bool CanWithdraw);

/// <summary>
/// One moderation-queue row, mapped from a shared <c>QueueRow</c> — the union of a
/// server-issued <c>ObligationAction</c> and a client-side post-decrypt local
/// detection (moderation.md § Layout &amp; flow). The badge label/icon/accent are
/// resolved from the shared <c>fauna_core::content_category</c> map — no per-app
/// category strings or colours (§ Categories &amp; enforcement). A <b>server</b> row
/// carries its enforcement <see cref="ActionLabel"/> (via <c>obligation_action_label</c>);
/// a <b>local</b> detection (<see cref="IsLocal"/>) has a <b>blank</b> action column —
/// never a fabricated action (§ Don't do these). <see cref="ContentId"/> is the content
/// ref the <c>train-correction-button</c> acts on (a server row trains
/// <c>fauna.moderation.train</c>; a local row removes the client-side flag).
/// <see cref="BadgeAttributes"/> is the badge's packed HelpText
/// (<see cref="ModerationViewModel.BadgeAttributes"/>).
/// </summary>
public record ModerationRow(
    string ContentId,
    string ContentType,
    string CategoryLabel,
    string CategoryIcon,
    string CategoryAccent,
    string ActionLabel,
    int ConfidencePercent,
    bool IsLocal,
    string BadgeAttributes,
    bool IsAppealable);
