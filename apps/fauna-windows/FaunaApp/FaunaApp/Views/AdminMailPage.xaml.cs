using System;
using System.Collections.Generic;
using System.ComponentModel;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Views;

/// <summary>
/// The flat <c>admin-mail</c> policy page (admin.md § 6 Mail; mail-policy-config.md
/// § Policy catalog Tier 2; ui.yaml <c>admin-mail</c>). A dumb renderer of the shared
/// <c>fauna_client_mail_settings::MailPolicyMachine</c> through the testable
/// <see cref="AdminMailViewModel"/> (FaunaApp.Core), consumed over the UniFFI
/// <c>IMailPolicyMachine</c> seam. The page builds the real machine over a connected
/// <see cref="FfiNestClient"/> (<see cref="FaunaFfiMethods.BuildMailPolicyMachine"/>),
/// hands it to the VM, then projects the VM's snapshot groups into named controls and
/// forwards each group's full-PUT Save gesture + the deployment-wide mail-enable toggle.
/// All projection / validation / WS-RPC sequencing lives in shared Rust (priority #2):
/// the <c>get_mail_config</c> hydrate + the seven <c>put_*</c> twins, the out-of-order
/// spam-threshold rejection, and the alias read/write twins; this page forwards gestures
/// and re-renders after each. No <c>ConfigureAwait(false)</c> in these handlers
/// (off-thread bound-state mutation throws a silent COMException). The
/// <c>admin-nav-back</c> affordance lives in the shared AdminShell header, NOT here.
/// Lifts the flat linux reference apps/fauna-linux/src/settings/admin_mail.rs; mirrors
/// <see cref="AdminAliasesPage"/> (machine build) + MailSpamPanel (named-control
/// RenderState + <c>_syncing</c> guard).
/// </summary>
public sealed partial class AdminMailPage : Page
{
    private ServiceClients? _clients;
    private AdminMailViewModel? _vm;

    // Set while RenderState programmatically updates a control whose change handler
    // dispatches (the mail-enable toggle), so its handler doesn't echo the change
    // back as an action. The Spam/Auth/etc. controls are read on Save (no per-change
    // dispatch), so they need no guard — but the guard also wraps their render so a
    // future bidirectional control stays safe.
    private bool _syncing;

    public AdminMailPage()
    {
        this.InitializeComponent();
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is ServiceClients clients)
        {
            _clients = clients;
        }
    }

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_clients is null) return;

        LoadProgress.IsActive = true;
        LoadProgress.Visibility = Visibility.Visible;
        try
        {
            if (_vm is null)
            {
                await EnsureViewModelAsync();
            }
            await _vm!.LoadCommand.ExecuteAsync(null);
            RenderState();
        }
        catch (Exception ex)
        {
            RenderError(Strings.Error(ex));
        }
        finally
        {
            LoadProgress.IsActive = false;
            LoadProgress.Visibility = Visibility.Collapsed;
        }
    }

    /// <summary>Build the mail-policy machine over the SHARED, already-connected +
    /// auto-reconnecting WS-RPC client (<see cref="INestRpcClient.BuildMailPolicyMachineAsync"/>,
    /// the <see cref="INestRpcClient.BuildDevicesMachineAsync"/> pattern) and wrap it
    /// in the VM. The put_* / get_mail_config / get_alias_policy kinds are Admin-class;
    /// the shared connection authenticates as the logged-in actor, which on this
    /// (am_i_admin-gated) page is the admin. Reusing the shared client avoids the prior
    /// per-page one-shot <see cref="FfiNestClient"/> whose transient connect failure
    /// surfaced as a page error (os-error-10061) while other pages recovered.</summary>
    private async Task EnsureViewModelAsync()
    {
        if (_vm is not null || _clients?.Rpc is null) return;
        // Build the mail-policy machine over the SHARED, already-connected +
        // auto-reconnecting WS-RPC client (the BuildDevicesMachineAsync pattern),
        // NOT a fresh one-shot FfiNestClient.Connect() — that second socket
        // surfaced a transient os-error-10061 as a page error while every other
        // page (on the shared client) recovered.
        var machine = await _clients.Rpc.BuildMailPolicyMachineAsync();
        _vm = new AdminMailViewModel(machine);
        _vm.PropertyChanged += ViewModel_PropertyChanged;
    }

    private void ViewModel_PropertyChanged(object? sender, PropertyChangedEventArgs e)
    {
        if (_vm is null) return;
        switch (e.PropertyName)
        {
            case nameof(AdminMailViewModel.IsLoading):
                LoadProgress.IsActive = _vm.IsLoading;
                LoadProgress.Visibility = _vm.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(AdminMailViewModel.Error):
                RenderError(_vm.Error);
                break;
        }
    }

    /// <summary>Reflect the VM's projected snapshot into the named controls. Called
    /// after the load + every Save / toggle. The mail-enable toggle is wrapped in the
    /// <c>_syncing</c> guard so reflecting persisted state never echoes back a dispatch;
    /// the read-on-Save controls need no guard but are set here too.</summary>
    private void RenderState()
    {
        if (_vm is null) return;

        RenderError(_vm.Error);

        _syncing = true;
        EnabledToggle.IsOn = _vm.MailEnabled;
        AutoEnableNewUsersToggle.IsOn = _vm.AutoEnableMailForNewUsers;
        _syncing = false;

        // ── Spam / inbound perimeter ──
        var s = _vm.Spam;
        ThresholdJunkInput.Text = s.MaxScoreBeforeSpamFolder.ToString();
        ThresholdRejectInput.Text = s.MaxScoreBeforeReject.ToString();
        DnsblInput.Text = string.Join("\n", s.DnsblServers);
        RejectNoRdnsToggle.IsOn = s.RejectNoRdns;
        GreylistEnabledToggle.IsOn = s.GreylistEnabled;
        GreylistDelayInput.Text = s.GreylistDelaySecs.ToString();
        MaxConnPerMinInput.Text = s.MaxConnPerMin.ToString();
        // Value-addressed, not position-addressed: the value set has ONE owner
        // (mail-policy-config.md § Architectural rules), the shared
        // FcrdnsModeOptions() catalog — never a hand-typed array here that a
        // grown ORDER could silently desync from.
        PopulateSelect(FcrdnsModeSelect, FaunaClientMailSettingsMethods.FcrdnsModeOptions(), s.FcrdnsMode, failClosed: "score_signal");
        HeloIdentityRequiredToggle.IsOn = s.HeloIdentityRequired;
        RejectFcrdnsFailToggle.IsOn = s.RejectFcrdnsFail;
        MaxMessageBytesInput.Text = s.MaxMessageBytes.ToString();
        BayesianWeightInput.Text = s.BayesianWeightMilli.ToString();
        BayesianMinSamplesInput.Text = s.BayesianMinSamples.ToString();
        BayesianFullConfidenceSamplesInput.Text = s.BayesianFullConfidenceSamples.ToString();
        TrainingHistoryRetentionInput.Text = s.TrainingHistoryRetentionDays.ToString();
        UnlistedRecipientPenaltyInput.Text = s.UnlistedRecipientPenalty.ToString();

        // ── Deployment-baseline publish outcome (empty until the admin publishes) ──
        // `Published` comes straight from the shared BaselinePublishValues, so the
        // client just picks the message + interpolates.
        var bp = _vm.BaselinePublish;
        if (bp is null)
        {
            PublishSpamBaselineResult.Text = "";
        }
        else
        {
            var msg = bp.Published
                ? Strings.Get("admin/mail_page/spam_baseline_published")
                    .Replace("{contributors}", bp.Contributors.ToString())
                    .Replace("{samples}", bp.SampleCount.ToString())
                : Strings.Get("admin/mail_page/spam_baseline_withheld")
                    .Replace("{contributors}", bp.Contributors.ToString());
            // The holder-side erosion count (silent-erosion fix, mail-spam.md
            // § Encrypted-mode interaction) — surfaced beside the published/
            // withheld message whenever the last run skipped anyone.
            if (bp.SkippedContributors > 0)
            {
                msg += " " + Strings.Get("admin/mail_page/spam_baseline_skipped_contributors")
                    .Replace("{count}", bp.SkippedContributors.ToString());
            }
            PublishSpamBaselineResult.Text = msg;
        }

        // ── Auth enforcement ──
        var a = _vm.Auth;
        EnforceDmarcToggle.IsOn = a.EnforceDmarc;
        EnforceDmarcQuarantineToggle.IsOn = a.EnforceDmarcQuarantine;
        EnforceSpfHardfailToggle.IsOn = a.EnforceSpfHardfail;
        EnforceDkimToggle.IsOn = a.EnforceDkim;
        LogOnlyToggle.IsOn = a.LogOnly;
        MaxFailuresInput.Text = a.MaxAuthFailuresPerMinute.ToString();
        MaxConnPerIpInput.Text = a.MaxConnPerIp.ToString();

        // ── Submission quotas ──
        var sub = _vm.Submission;
        SubmissionMaxPerDayInput.Text = sub.MaxPerDay.ToString();
        SubmissionMaxRecipientsInput.Text = sub.MaxRecipientsPerMessage.ToString();

        // ── IMAP server policy ──
        var i = _vm.Imap;
        ImapIdleTimeoutInput.Text = i.IdleTimeoutSecs.ToString();
        ImapTombstoneRetentionInput.Text = i.TombstoneRetentionDays.ToString();
        // Same rule as the fcrdns select above — value-addressed off the shared
        // ImapDeleteNonemptyOptions() catalog.
        PopulateSelect(ImapDeleteNonemptySelect, FaunaClientMailSettingsMethods.ImapDeleteNonemptyOptions(), i.DeleteNonempty, failClosed: "forbidden");
        ImapBodystructureCacheInput.Text = i.BodystructureCacheMax.ToString();
        ImapStorageBytesInput.Text = i.StorageBytesDefault.ToString();
        ImapMessageCountInput.Text = i.MessageCountDefault.ToString();

        // ── Outbound delivery ──
        var o = _vm.Outbound;
        OutboundRetryScheduleInput.Text = string.Join("\n", o.RetrySchedule.Select(v => v.ToString()));
        OutboundPermfailTimeoutInput.Text = o.PermanentFailureTimeoutHours.ToString();
        OutboundDelayWarningInput.Text = o.DelayWarningAtHours.ToString();
        OutboundNdrRateLimitInput.Text = o.NdrRateLimitDays.ToString();
        OutboundSuppressNdrSpfToggle.IsOn = o.SuppressNdrSpfHardfail;
        OutboundSuppressNdrDmarcToggle.IsOn = o.SuppressNdrDmarcReject;
        OutboundPostmasterCcToggle.IsOn = o.PostmasterCcBounces;
        OutboundTlsrptSendToggle.IsOn = o.TlsrptSendReports;
        OutboundIpv6Toggle.IsOn = o.Ipv6Enabled;
        OutboundTreat5xxInput.Text = string.Join("\n", o.Treat5xxAsTransient);

        // ── Aliases ──
        var al = _vm.Alias;
        AliasExactMaxInput.Text = al.ExactAliasesMax.ToString();
        AliasReservedInput.Text = string.Join("\n", al.ReservedLocalParts);
        AliasSubaddressingToggle.IsOn = al.SubaddressingEnabled;
        AliasWildcardPrefixToggle.IsOn = al.WildcardPrefixEnabled;
    }

    /// <summary>Render the VM's last action error onto the app-wide
    /// <c>error-message</c> InfoBar + the state-protocol error surface.</summary>
    private void RenderError(string? msg)
    {
        var empty = string.IsNullOrEmpty(msg);
        ErrorBar.Message = msg ?? string.Empty;
        ErrorBar.IsOpen = !empty;
        ErrorTextMirror.Text = msg ?? " ";
        App.CurrentErrorMessage = empty ? null : msg;
    }

    // ── Mail-enable master toggle (set_mail_enabled) ──
    private async void EnabledToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing || _vm is null || sender is not ToggleSwitch sw) return;
        await _vm.SetMailEnabledAsync(sw.IsOn);
        RenderState();
    }

    // ── Auto-enable-for-new-users toggle (set_auto_enable_mail_for_new_users) ──
    private async void AutoEnableNewUsersToggle_Toggled(object sender, RoutedEventArgs e)
    {
        if (_syncing || _vm is null || sender is not ToggleSwitch sw) return;
        await _vm.SetAutoEnableMailForNewUsersAsync(sw.IsOn);
        RenderState();
    }

    // ── Spam group Save → full PUT ──
    private async void SpamSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var p = _vm.Spam;
        var values = new SpamPolicyValues(
            ParseUint(ThresholdJunkInput.Text, p.MaxScoreBeforeSpamFolder),
            ParseUint(ThresholdRejectInput.Text, p.MaxScoreBeforeReject),
            ReadLines(DnsblInput.Text),
            RejectNoRdnsToggle.IsOn,
            GreylistEnabledToggle.IsOn,
            ParseUint(GreylistDelayInput.Text, p.GreylistDelaySecs),
            ParseUint(MaxConnPerMinInput.Text, p.MaxConnPerMin),
            SelectedValue(FcrdnsModeSelect) ?? p.FcrdnsMode,
            HeloIdentityRequiredToggle.IsOn,
            RejectFcrdnsFailToggle.IsOn,
            ParseUint(MaxMessageBytesInput.Text, p.MaxMessageBytes),
            ParseUint(BayesianWeightInput.Text, p.BayesianWeightMilli),
            ParseUint(BayesianMinSamplesInput.Text, p.BayesianMinSamples),
            ParseUint(BayesianFullConfidenceSamplesInput.Text, p.BayesianFullConfidenceSamples),
            ParseUint(TrainingHistoryRetentionInput.Text, p.TrainingHistoryRetentionDays),
            ParseUint(UnlistedRecipientPenaltyInput.Text, p.UnlistedRecipientPenalty),
            p.BaselineStandingPublish);
        await _vm.SaveSpamAsync(values);
        RenderState();
    }

    // ── Publish deployment baseline → PublishSpamBaseline (no gather) ──
    private async void PublishSpamBaselineButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.PublishSpamBaselineAsync();
        RenderState();
    }

    // ── Auth group Save → full PUT ──
    private async void AuthSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var p = _vm.Auth;
        var values = new AuthPolicyValues(
            EnforceDmarcToggle.IsOn,
            EnforceDmarcQuarantineToggle.IsOn,
            EnforceSpfHardfailToggle.IsOn,
            EnforceDkimToggle.IsOn,
            LogOnlyToggle.IsOn,
            ParseUint(MaxFailuresInput.Text, p.MaxAuthFailuresPerMinute),
            // Per-IP concurrent-conn cap (0 = disabled); falls back to the loaded value.
            ParseUint(MaxConnPerIpInput.Text, p.MaxConnPerIp));
        await _vm.SaveAuthAsync(values);
        RenderState();
    }

    // ── Submission group Save → full PUT ──
    private async void SubmissionSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var p = _vm.Submission;
        var values = new SubmissionPolicyValues(
            ParseUint(SubmissionMaxPerDayInput.Text, p.MaxPerDay),
            ParseUint(SubmissionMaxRecipientsInput.Text, p.MaxRecipientsPerMessage));
        await _vm.SaveSubmissionAsync(values);
        RenderState();
    }

    // ── IMAP group Save → full PUT ──
    private async void ImapSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var p = _vm.Imap;
        var values = new ImapPolicyValues(
            ParseUint(ImapIdleTimeoutInput.Text, p.IdleTimeoutSecs),
            ParseUint(ImapTombstoneRetentionInput.Text, p.TombstoneRetentionDays),
            SelectedValue(ImapDeleteNonemptySelect) ?? p.DeleteNonempty,
            ParseUint(ImapBodystructureCacheInput.Text, p.BodystructureCacheMax),
            ParseUlong(ImapStorageBytesInput.Text, p.StorageBytesDefault),
            ParseUint(ImapMessageCountInput.Text, p.MessageCountDefault));
        await _vm.SaveImapAsync(values);
        RenderState();
    }

    // ── Outbound group Save → full PUT. postmaster-cc is read-only (project policy
    // never CC); the disabled toggle keeps the rendered persisted value unchanged. ──
    private async void OutboundSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var p = _vm.Outbound;
        var values = new OutboundPolicyValues(
            ReadUlongLines(OutboundRetryScheduleInput.Text, p.RetrySchedule),
            ParseUint(OutboundPermfailTimeoutInput.Text, p.PermanentFailureTimeoutHours),
            ParseUint(OutboundDelayWarningInput.Text, p.DelayWarningAtHours),
            ParseUint(OutboundNdrRateLimitInput.Text, p.NdrRateLimitDays),
            OutboundSuppressNdrSpfToggle.IsOn,
            OutboundSuppressNdrDmarcToggle.IsOn,
            OutboundPostmasterCcToggle.IsOn,
            OutboundTlsrptSendToggle.IsOn,
            OutboundIpv6Toggle.IsOn,
            ReadLines(OutboundTreat5xxInput.Text));
        await _vm.SaveOutboundAsync(values);
        RenderState();
    }

    // ── Aliases group Save → full PUT. An empty reserved-local-parts box clears the
    // reservation (full-replace with []), matching the wire semantics. ──
    private async void AliasSaveButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        var p = _vm.Alias;
        var values = new AliasPolicyValues(
            ParseUint(AliasExactMaxInput.Text, p.ExactAliasesMax),
            ReadLines(AliasReservedInput.Text),
            AliasSubaddressingToggle.IsOn,
            AliasWildcardPrefixToggle.IsOn);
        await _vm.SaveAliasAsync(values);
        RenderState();
    }

    /// <summary>Parse a numeric TextBox as <c>uint</c>, falling back to the persisted
    /// value on an empty/unparseable edit (never silently zeroes a knob). Single-sourced
    /// on the shared <c>fauna_core::format::parse_count</c> validator (trim, u32, accept
    /// leading '+', reject empty/negative/fractional/overflow) over <c>ParseCount</c> — the
    /// same family as the consumed <c>ParseCap</c> / <c>ParsePort</c> (priority #1/#2/#4).</summary>
    private static uint ParseUint(string text, uint prev)
        => uniffi.fauna_ffi.FaunaFfiMethods.ParseCount(text ?? "") ?? prev;

    /// <summary>Parse a numeric TextBox as <c>ulong</c> (the IMAP storage ceiling), falling
    /// back to the persisted value. Single-sourced on the shared
    /// <c>fauna_core::format::parse_count_u64</c> validator over <c>ParseCountU64</c>.</summary>
    private static ulong ParseUlong(string text, ulong prev)
        => uniffi.fauna_ffi.FaunaFfiMethods.ParseCountU64(text ?? "") ?? prev;

    /// <summary>Split a multiline TextBox into one trimmed non-blank string per line
    /// (full-replace; an empty box yields an empty list).</summary>
    private static IReadOnlyList<string> ReadLines(string text)
        => (text ?? string.Empty)
            .Split('\n')
            .Select(l => l.Trim())
            .Where(l => l.Length > 0)
            .ToList();

    /// <summary>Split a multiline TextBox into a <c>Vec&lt;u64&gt;</c> (one delay-seconds
    /// value per non-blank line; the outbound retry schedule). Falls back to the whole
    /// previous list if any line fails to parse — a full PUT never sends a partially
    /// parsed schedule (mirrors the linux read_u64_lines).</summary>
    private static IReadOnlyList<ulong> ReadUlongLines(string text, IReadOnlyList<ulong> prev)
    {
        var lines = (text ?? string.Empty)
            .Split('\n')
            .Select(l => l.Trim())
            .Where(l => l.Length > 0)
            .ToList();
        var parsed = new List<ulong>(lines.Count);
        foreach (var line in lines)
        {
            if (!ulong.TryParse(line, out var v)) return prev;
            parsed.Add(v);
        }
        return parsed;
    }

    /// <summary>Populate <paramref name="combo"/> from a shared-Rust
    /// <see cref="ReachPolicyOption"/> catalog and select <paramref name="selected"/>
    /// by VALUE, never by position — the same shape <c>FamilyPage.PopulateSelect</c>
    /// uses for its own enumerated knobs. An unmatched value (a stored token this
    /// build's catalog no longer lists) selects <paramref name="failClosed"/> instead
    /// of leaving nothing selected; each call site's <paramref name="failClosed"/> is
    /// that field's own established default, not necessarily the strictest option
    /// (fcrdns' is the observability-first middle default; delete-nonempty's is the
    /// safer one) — both mirror `admin_policy.rs`'s own `from_wire_or_default`.</summary>
    private static void PopulateSelect(ComboBox combo, ReachPolicyOption[] options, string selected, string failClosed)
    {
        combo.Items.Clear();
        foreach (var opt in options)
        {
            var item = new ComboBoxItem { Content = Strings.Resolve(opt.@label), Tag = opt.@value };
            combo.Items.Add(item);
            if (opt.@value == selected) combo.SelectedItem = item;
        }
        if (combo.SelectedItem is null)
            foreach (var obj in combo.Items)
                if (obj is ComboBoxItem { Tag: string t } item && t == failClosed)
                {
                    combo.SelectedItem = item;
                    break;
                }
    }

    /// <summary>The wire value of <paramref name="combo"/>'s selected item (a
    /// <see cref="PopulateSelect"/>-populated ComboBox's <c>Tag</c>), or <c>null</c>
    /// if nothing is selected — the caller falls back to the persisted value, the
    /// same "never send a value we can't read back" rule the numeric parsers follow.</summary>
    private static string? SelectedValue(ComboBox combo) => (combo.SelectedItem as ComboBoxItem)?.Tag as string;
}
