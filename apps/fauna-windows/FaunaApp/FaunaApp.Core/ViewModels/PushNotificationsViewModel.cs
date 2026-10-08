using System;
using System.Threading.Tasks;
using CommunityToolkit.Mvvm.ComponentModel;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Settings → Account → Push notifications (<c>settings.md</c> § Push notifications): one
/// toggle — this install's opt-in bit, never the OS notification permission — and one
/// inline line. Everything it decides is shared Rust: the toggle drives the shared
/// registration machine (<see cref="IFfiPushRegistration"/>'s <c>Enable</c> /
/// <c>Disable</c>, <see cref="PushSession"/> names windows' inputs), and the line's
/// standing cause is <c>push_standing_failure</c> — the rule tui renders.
///
/// <para><see cref="OptedIn"/> is written only from the stored record (load, and after
/// each toggle's attempt), never from the click, so it is evidence of the persisted bit:
/// a failed enable reads off again, and a disable reads off even when the nest was
/// unreachable (the shared machine clears the bit first).</para>
/// </summary>
public sealed partial class PushNotificationsViewModel : ObservableObject
{
    private readonly Func<FfiPushIntent> _readIntent;
    private readonly Func<Task<IFfiPushRegistration?>> _buildRegistration;
    private readonly Action _clearOptIn;
    private readonly IAgentStatusProbe _agent;
    private readonly Func<string> _localBuildVersion;

    /// <summary>The last toggle's failure — takes the line over the standing cause until
    /// a toggle succeeds.</summary>
    private string? _toggleFailure;

    /// <summary>The install's stored opt-in bit — what the toggle shows.</summary>
    [ObservableProperty] private bool _optedIn;

    /// <summary>The <c>push-notifications-error</c> line, or <c>null</c> when nothing is
    /// wrong (the element is then not rendered).</summary>
    [ObservableProperty] private string? _lineText;

    /// <summary>A toggle is in flight.</summary>
    [ObservableProperty] private bool _busy;

    /// <param name="buildRegistration">This install's registration under the signed-in
    /// actor, or <c>null</c> when there is no session or device id to build it on.</param>
    internal PushNotificationsViewModel(
        Func<Task<IFfiPushRegistration?>> buildRegistration,
        IAgentStatusProbe agent,
        Func<FfiPushIntent>? readIntent = null,
        Action? clearOptIn = null,
        Func<string>? localBuildVersion = null)
    {
        _buildRegistration = buildRegistration;
        _agent = agent;
        _readIntent = readIntent ?? (() => FaunaFfiMethods.PushIntent(PushSession.IntentPath));
        _clearOptIn = clearOptIn ?? (() => FaunaFfiMethods.PushClearOptIn(PushSession.IntentPath));
        _localBuildVersion = localBuildVersion ?? FaunaFfiMethods.FaunaFfiBuildVersion;
    }

    /// <summary>Read the stored record — local, so the toggle paints before the agent
    /// answers.</summary>
    public void Load() => OptedIn = _readIntent().@optedIn;

    /// <summary><see cref="Load"/>, then the line from the agent's current answer.</summary>
    public async Task LoadAsync()
    {
        Load();
        await RefreshLineAsync();
    }

    /// <summary>The toggle: on → enable, off → disable. A failure lands on the line and
    /// leaves <see cref="OptedIn"/> at whatever the store now holds.</summary>
    public async Task SetOptInAsync(bool on)
    {
        Busy = true;
        string? failure = null;
        try
        {
            var registration = await _buildRegistration();
            if (registration is null)
            {
                // No session to reach the nest through. Off still stays off: the bit
                // clears with no connection, keeping the actor record so a later
                // leave-drop still removes the row (the shared `push_clear_opt_in`).
                if (!on) _clearOptIn();
                failure = Strings.Get("settings/push_notifications/update_failed");
            }
            else if (on)
            {
                await registration.Enable(PushSession.WsDevice());
            }
            else
            {
                await registration.Disable();
            }
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(PushNotificationsViewModel), $"toggle failed: {ex.Message}");
            failure = Strings.Error(ex);
        }
        finally
        {
            Busy = false;
        }

        _toggleFailure = failure;
        OptedIn = _readIntent().@optedIn;
        await RefreshLineAsync();
    }

    /// <summary>Re-read the agent and repaint the line — the page calls this on its own
    /// cadence, since the agent can come and go while the page is open.</summary>
    public async Task RefreshLineAsync()
    {
        if (_toggleFailure is not null)
        {
            LineText = _toggleFailure;
            return;
        }
        FfiPushStandingFailure? standing = null;
        try
        {
            var status = await _agent.ProbeAsync(_localBuildVersion());
            standing = FaunaFfiMethods.PushStandingFailure(
                OptedIn, status.@state != FfiAgentHealthState.NotRunning, status.@notificationSink);
        }
        catch (Exception ex)
        {
            ShellLog.Warn(nameof(PushNotificationsViewModel), $"agent status read failed: {ex.Message}");
        }
        LineText = standing switch
        {
            FfiPushStandingFailure.AgentUnreachable => Strings.Get("settings/push_notifications/agent_unreachable"),
            FfiPushStandingFailure.NoSink => Strings.Get("settings/push_notifications/no_sink"),
            _ => null,
        };
    }
}
