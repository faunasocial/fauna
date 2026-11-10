using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The account controls of the mail-settings page: the receive, send and scan-result-row
/// switches (mail-policy-config.md § Tier 3 — per-account), the automatic reply
/// (email-filters.md § The auto-respond setting) and the quota readouts
/// (imap-server.md § Composition with submission quota); ui ids
/// <c>mail-settings-receive-toggle</c> / <c>-send-toggle</c> /
/// <c>-scan-result-visible-toggle</c> /
/// <c>-autorespond-toggle</c> / <c>-autorespond-message-input</c> /
/// <c>-mailbox-quota</c> / <c>-submission-quota</c> / <c>-over-quota-notice</c>. A dumb
/// projection over the shared draft check and quota wording and the
/// <see cref="INestRpcClient"/> calls: non-optimistic — every field is what the nest
/// last answered, never the gesture — and null until the first read, so nothing paints
/// before it. Mirrors tui's <c>MailAccountRead</c> and apple's <c>MailSettingsVM</c>.
/// </summary>
internal sealed class MailAccountViewModel
{
    private readonly INestRpcClient _rpc;

    public MailAccountViewModel(INestRpcClient rpc) => _rpc = rpc;

    /// <summary>The receive switch as the nest kept it; null until read.</summary>
    public bool? ReceiveEnabled { get; private set; }

    /// <summary>The send switch as the nest kept it; null until read.</summary>
    public bool? SendEnabled { get; private set; }

    /// <summary>Whether received messages show the scan-result row, as the nest kept it;
    /// null until read. Only the switch lives here — the shared conversations projection
    /// does the hiding (mail-content-scanning.md § Per-account opt-out for visible scan
    /// details).</summary>
    public bool? ScanResultVisible { get; private set; }

    /// <summary>Whether the automatic reply is on; null until read.</summary>
    public bool? AutorespondEnabled { get; private set; }

    /// <summary>The reply's stored words. Survives turning it off: the rule's body goes
    /// with the rule, and the page keeps the words so turning it back on sends them.</summary>
    public string AutorespondMessage { get; private set; } = string.Empty;

    /// <summary>The <c>mail-settings-mailbox-quota</c> readout; null until read.</summary>
    public string? MailboxQuotaText { get; private set; }

    /// <summary>The <c>mail-settings-submission-quota</c> readout; null until read.</summary>
    public string? SubmissionQuotaText { get; private set; }

    /// <summary>The <c>mail-settings-over-quota-notice</c> sentence; null unless a limit
    /// has run out.</summary>
    public string? OverQuotaNotice { get; private set; }

    /// <summary>The newest refusal / failure for the page's <c>error-message</c>, or null.</summary>
    public string? Error { get; private set; }

    /// <summary>Read the switches, the reply and the quotas (replay-safe pure reads). A
    /// failed part keeps what it had; the first failure names itself.</summary>
    public async Task LoadAsync()
    {
        Error = null;
        try { ApplySwitches(await _rpc.MailSwitchesGetAsync()); }
        catch (Exception ex) { Error = Strings.Error(ex); }
        try { ApplyAutorespond(await _rpc.AutorespondGetAsync()); }
        catch (Exception ex) { Error ??= Strings.Error(ex); }
        try { ApplyQuotas(await _rpc.MailQuotasGetAsync()); }
        catch (Exception ex) { Error ??= Strings.Error(ex); }
    }

    /// <summary>Set the receive switch alone; inert until the first read.</summary>
    public Task SetReceiveAsync(bool on) =>
        ReceiveEnabled is null ? Task.CompletedTask : WriteSwitchesAsync(on, null);

    /// <summary>Set the send switch alone; inert until the first read.</summary>
    public Task SetSendAsync(bool on) =>
        SendEnabled is null ? Task.CompletedTask : WriteSwitchesAsync(null, on);

    private async Task WriteSwitchesAsync(bool? receive, bool? send)
    {
        Error = null;
        try { ApplySwitches(await _rpc.MailSwitchesSetAsync(receive, send)); }
        catch (Exception ex) { Error = Strings.Error(ex); }
    }

    /// <summary>Set the scan-result-row switch alone; inert until the first read.</summary>
    public async Task SetScanResultVisibleAsync(bool on)
    {
        if (ScanResultVisible is null) return;
        Error = null;
        try { ApplySwitches(await _rpc.MailScanResultVisibleSetAsync(on)); }
        catch (Exception ex) { Error = Strings.Error(ex); }
    }

    /// <summary>The reply's toggle: off clears it, on commits <paramref name="draft"/>
    /// (which turns it on); inert until the first read.</summary>
    public async Task SetAutorespondAsync(bool on, string draft)
    {
        if (AutorespondEnabled is null) return;
        if (on)
        {
            await CommitAutorespondMessageAsync(draft);
            return;
        }
        Error = null;
        try
        {
            await _rpc.AutorespondClearAsync();
            ApplyAutorespond(await _rpc.AutorespondGetAsync());
        }
        catch (Exception ex) { Error = Strings.Error(ex); }
    }

    /// <summary>Enter on the reply's message: the shared check refuses a blank draft on
    /// the page and sends nothing; otherwise the reply is set (and so turned on) and
    /// re-read.</summary>
    public async Task CommitAutorespondMessageAsync(string draft)
    {
        Error = null;
        var outcome = FaunaClientMailSettingsMethods.AutorespondMessageDraftOutcome(draft);
        if (outcome is AutorespondMessageDraftOutcome.Refused refused)
        {
            Error = Strings.Resolve(refused.message);
            return;
        }
        try
        {
            await _rpc.AutorespondSetAsync(((AutorespondMessageDraftOutcome.Set)outcome).text);
            ApplyAutorespond(await _rpc.AutorespondGetAsync());
        }
        catch (Exception ex) { Error = Strings.Error(ex); }
    }

    private void ApplySwitches(MailSwitches switches)
    {
        ReceiveEnabled = switches.receiveEnabled;
        SendEnabled = switches.sendEnabled;
        ScanResultVisible = switches.scanResultVisible;
    }

    private void ApplyAutorespond(FfiAutoRespond reply)
    {
        AutorespondEnabled = reply.enabled;
        // An off reply carries no body: keep the words rather than blanking them.
        if (reply.enabled) AutorespondMessage = reply.message;
    }

    private void ApplyQuotas(MailQuotas quotas)
    {
        MailboxQuotaText = Strings.MailQuotaText(FaunaClientMailSettingsMethods.MailQuotaDisplayOf(quotas));
        SubmissionQuotaText = Strings.Resolve(FaunaClientMailSettingsMethods.MailSubmissionQuotaDisplayOf(quotas));
        OverQuotaNotice = FaunaClientMailSettingsMethods.MailOverQuotaNoticeOf(quotas) is { } notice
            ? Strings.Resolve(notice)
            : null;
    }
}
