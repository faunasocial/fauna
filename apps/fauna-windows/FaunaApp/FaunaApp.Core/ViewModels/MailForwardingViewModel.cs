using System;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_client_mail_settings;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Forwarding section of the mail-settings page: the per-account
/// "forward all incoming mail to" address and the hourly forwarding limit
/// (mail-forwarding.md § Per-account "forward all", § Per-account forward
/// rate-limit; ui ids <c>mail-settings-forward-all-to-input</c> /
/// <c>mail-settings-forward-per-hour-input</c>). A dumb projection over the
/// shared draft parsers (<c>forward_all_to_draft_outcome</c>,
/// <c>forward_per_hour_draft_outcome</c>, <c>forward_per_hour_hint</c>) and the
/// four <see cref="INestRpcClient"/> calls — the draft is checked in shared Rust
/// first, a refusal names its reason in <see cref="Error"/> and sends nothing, and
/// each field repaints from the value the nest kept, never the keystrokes. Mirrors
/// linux's <c>hydrate_forwarding</c> / <c>commit_forward_*</c>.
/// </summary>
internal sealed class MailForwardingViewModel
{
    private readonly INestRpcClient _rpc;
    // The admin ceiling the nest returned; null until the first limit read resolves,
    // which keeps a limit commit inert (linux's forward_ceiling).
    private uint? _ceiling;

    public MailForwardingViewModel(INestRpcClient rpc) => _rpc = rpc;

    /// <summary>The forward-all field's persisted text (empty = forwarding off).</summary>
    public string ForwardAllToText { get; private set; } = string.Empty;

    /// <summary>The hourly-limit field's persisted text.</summary>
    public string ForwardPerHourText { get; private set; } = string.Empty;

    /// <summary>The limit's explainer, which names the ceiling; empty until the first read.</summary>
    public string ForwardPerHourHint { get; private set; } = string.Empty;

    /// <summary>The newest refusal / failure for the page's <c>error-message</c>, or null.</summary>
    public string? Error { get; private set; }

    /// <summary>Read both values (replay-safe pure reads) and apply them.</summary>
    public async Task LoadAsync()
    {
        Error = null;
        try { ApplyAddress(await _rpc.ForwardAllToGetAsync()); }
        catch (Exception ex) { Error = Strings.Error(ex); }
        try { ApplyLimit(await _rpc.ForwardPerHourGetAsync()); }
        catch (Exception ex) { Error ??= Strings.Error(ex); }
    }

    /// <summary>Commit the forward-all draft: blank clears, a refused draft sends nothing.</summary>
    public async Task CommitForwardAllToAsync(string draft)
    {
        Error = null;
        var outcome = FaunaClientMailSettingsMethods.ForwardAllToDraftOutcome(draft);
        string? address;
        switch (outcome)
        {
            case ForwardAllToDraftOutcome.Refused refused:
                Error = Strings.Resolve(refused.message);
                return;
            case ForwardAllToDraftOutcome.Set set:
                address = set.address;
                break;
            default:
                address = null;
                break;
        }
        try { ApplyAddress(await _rpc.ForwardAllToSetAsync(address)); }
        catch (Exception ex) { Error = Strings.Error(ex); }
    }

    /// <summary>Commit the hourly-limit draft, bounded by the ceiling the nest returned;
    /// inert until the first read has resolved.</summary>
    public async Task CommitForwardPerHourAsync(string draft)
    {
        Error = null;
        if (_ceiling is not { } ceiling) return;
        var outcome = FaunaClientMailSettingsMethods.ForwardPerHourDraftOutcome(draft, ceiling);
        if (outcome is ForwardPerHourDraftOutcome.Refused refused)
        {
            Error = Strings.Resolve(refused.message);
            return;
        }
        try { ApplyLimit(await _rpc.ForwardPerHourSetAsync(((ForwardPerHourDraftOutcome.Value)outcome).value)); }
        catch (Exception ex) { Error = Strings.Error(ex); }
    }

    private void ApplyAddress(string? address) => ForwardAllToText = address ?? string.Empty;

    private void ApplyLimit(uniffi.fauna_ffi.ForwardPerHour reply)
    {
        _ceiling = reply.ceiling;
        ForwardPerHourText = reply.value.ToString();
        ForwardPerHourHint = Strings.Resolve(FaunaClientMailSettingsMethods.ForwardPerHourHint(reply.ceiling));
    }
}
