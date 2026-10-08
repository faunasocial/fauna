using CommunityToolkit.Mvvm.ComponentModel;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The newer-version check — the windows leg of <c>installers/README.md</c> § Knowing a
/// newer version is out: the app answers when you ask (Settings → General's
/// <c>settings-check-updates-button</c>), looks once per sign-in, and only ever TELLS
/// you (<c>update-available-notice</c>). It never polls on a timer, never downloads and
/// never replaces itself, and has no toggle.
///
/// <para>Everything that decides the answer is shared Rust: the round trip, the feed's
/// shape, the semver rule and the release page all come through the <c>fauna-ffi</c>
/// face of <c>fauna_client::update_look</c> (<c>CheckForNewerRelease</c>,
/// <c>LookAtSignIn</c>, <c>ReleaseFeedOrigin</c>) — the calls linux and tui make
/// directly and macOS makes over the same face (<c>UpdateCheck.swift</c>). This type
/// keeps no feed URL and no round trip of its own; it only holds what the page paints.</para>
///
/// <para>One instance per process (<see cref="Shared"/>), written by the asked check and
/// by the sign-in look, so both paint the same notice in the same place. The look can
/// land before Settings was ever opened; the page reads <see cref="Notice"/> when it
/// loads. Change notifications can arrive off the UI thread (the e2e login seam runs
/// the look from the agent's command thread), so the page marshals them.</para>
/// </summary>
public sealed partial class UpdateCheck : ObservableObject
{
    /// <summary>The asked check's state, carried in the button's own label — the walk
    /// observes resolution as the label leaving "Checking…".</summary>
    public enum CheckPhase { Idle, Checking, Newer, UpToDate, Failed }

    /// <summary>How long the label holds the check's answer before the button reads
    /// "Check for Updates" again (linux's and macOS's three seconds).</summary>
    private static readonly TimeSpan AnswerHold = TimeSpan.FromSeconds(3);

    private static readonly Lazy<UpdateCheck> _shared = new(() => new UpdateCheck(FeedOrigin(), AnswerHold));

    /// <summary>The process's one instance.</summary>
    public static UpdateCheck Shared => _shared.Value;

    private readonly string _feedOrigin;
    private readonly TimeSpan _answerHold;
    // Which asked check the pending label reset belongs to: a reset never clobbers a
    // newer check's state.
    private int _generation;
    private string? _newerVersion;

    internal UpdateCheck(string feedOrigin, TimeSpan answerHold)
    {
        _feedOrigin = feedOrigin;
        _answerHold = answerHold;
    }

    [ObservableProperty]
    [NotifyPropertyChangedFor(nameof(ButtonLabel))]
    [NotifyPropertyChangedFor(nameof(IsChecking))]
    private CheckPhase _phase = CheckPhase.Idle;

    /// <summary>The notice's text once the asked check or the sign-in look found a newer
    /// release (session-local, never persisted); <c>null</c> paints nothing.</summary>
    [ObservableProperty] private string? _notice;

    /// <summary>The version this build is — the one workspace product version every app
    /// and the nest share (<c>product-version.md</c> § The model).</summary>
    public string RunningVersion { get; } = FaunaFfiMethods.FaunaFfiBuildVersion();

    public bool IsChecking => Phase == CheckPhase.Checking;

    /// <summary>The button's label for the current phase, in the shared strings.</summary>
    public string ButtonLabel => Phase switch
    {
        CheckPhase.Checking => Strings.Get("common/checking"),
        CheckPhase.Newer => Strings.Format("settings/general_page/update_available", _newerVersion ?? string.Empty),
        CheckPhase.UpToDate => Strings.Get("settings/up_to_date"),
        CheckPhase.Failed => Strings.Get("settings/check_failed"),
        _ => Strings.Get("settings/check_for_updates"),
    };

    /// <summary>
    /// The asked check: newer, up to date or failed — a failed round trip reads as
    /// failed, never as "up to date". The label holds the answer for three seconds,
    /// then the button is ready to ask again.
    /// </summary>
    public async Task CheckAsync()
    {
        if (IsChecking) return;
        var generation = ++_generation;
        Phase = CheckPhase.Checking;
        NewerReleaseCheck answer;
        try
        {
            answer = await FaunaFfiMethods.CheckForNewerRelease(_feedOrigin, UserAgent);
        }
        catch (Exception)
        {
            // A panic across the FFI boundary is a check that did not answer.
            answer = new NewerReleaseCheck.Failed();
        }
        switch (answer)
        {
            case NewerReleaseCheck.Newer newer:
                _newerVersion = newer.release.version;
                Phase = CheckPhase.Newer;
                Show(newer.release);
                break;
            case NewerReleaseCheck.UpToDate:
                Phase = CheckPhase.UpToDate;
                break;
            default:
                Phase = CheckPhase.Failed;
                break;
        }
        _ = ReturnToIdleAsync(generation);
    }

    /// <summary>
    /// The once-per-sign-in look — called from each path that builds a signed-in
    /// session. Silent unless a newer release is out: a failed look paints nothing.
    /// </summary>
    public async Task LookOnceAtSignInAsync()
    {
        try
        {
            if (await FaunaFfiMethods.LookAtSignIn(_feedOrigin, UserAgent) is { } release)
                Show(release);
        }
        catch (Exception)
        {
            // Silent by rule, a panic across the FFI boundary included.
        }
    }

    private void Show(NewerRelease release) =>
        Notice = Strings.Format("settings/update_available_notice", release.version, release.releasePageUrl);

    private async Task ReturnToIdleAsync(int generation)
    {
        await Task.Delay(_answerHold);
        if (generation == _generation && !IsChecking) Phase = CheckPhase.Idle;
    }

    private string UserAgent => $"fauna-windows/{RunningVersion}";

    /// <summary>Production's feed origin, or — only in a test-capable build under e2e
    /// automation — the harness's stub feed (convention 15: <see cref="E2eEnv"/>'s
    /// production twin answers <c>null</c> with no variable name compiled in).</summary>
    private static string FeedOrigin() =>
        E2eEnv.Bridge is not null && E2eEnv.ReleaseFeedUrl is { Length: > 0 } stub
            ? stub
            : FaunaFfiMethods.ReleaseFeedOrigin();
}
