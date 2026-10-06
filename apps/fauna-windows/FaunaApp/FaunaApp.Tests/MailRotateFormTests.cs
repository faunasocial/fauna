using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_mail_settings;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The rotate-keys form's hold-open contract (<c>mail-settings.md</c> § Element
/// visibility: <c>mail-rotate-keys-progress-indicator</c> shows "during multi-step
/// rotation"; § Architectural rules 1 and 5: same shape on all 7 apps, no second
/// rotation racing the first). <see cref="MailRotateForm"/> is the
/// <c>FaunaApp.Core</c> seam <c>MailSettingsPanel.RotateConfirm_Click</c> drives —
/// the panel is code-behind with no view model, so the ORDER of the confirm
/// (paint progress + disable → await the rotation → collapse → re-enable) lives
/// here, where a unit test can pin it.
///
/// <para>The regression these lock is the collapse-first shape: the form was
/// collapsed BEFORE the dispatch was awaited, so nothing on screen showed the
/// rotation running and the e2e barrier
/// <c>MailSettingsActions.wait_for_rotation_to_finish</c> passed at once (a relaunch
/// could then cut the rotation short).</para>
/// </summary>
[Collection("StringsGlobal")]
public class MailRotateFormTests
{
    private sealed class FakeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["settings/mail/status_rotation"] = "{count} remaining",
        };

        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    public MailRotateFormTests() => Strings.Initialize(new FakeLocalizer());

    private static SettingsStatus Idle => new SettingsStatus.Idle();

    private static MailCredentialSummary Cred(string id, bool revoked = false) =>
        new(id, id, CredentialKind.Plain, 0UL, id, revoked);

    private static MailSettingsSnapshot Snapshot(params MailCredentialSummary[] credentials) =>
        new(
            enabled: true,
            caldavEnabled: false,
            carddavEnabled: false,
            servesWebdavSet: false,
            credentialManagementReachable: true,
            servingEnabled: true,
            credentials: credentials,
            pendingRotation: null,
            mua: new MuaInstructions("", 993, "", 465, "", 443, "", "", "", ""),
            status: Idle,
            error: null);

    private static string Painted(MailRotateFormPaint p) =>
        $"paint {p.ProgressText}|{(p.ControlsEnabled ? "enabled" : "disabled")}";

    [Fact]
    public async Task Confirm_HoldsTheFormOpenWithProgressUntilTheRotationReturns()
    {
        var form = new MailRotateForm();
        var events = new List<string>();
        var release = new TaskCompletionSource();

        var confirm = form.ConfirmAsync(
            rewrapCount: 2,
            status: () => Idle,
            paint: p => events.Add(Painted(p)),
            dispatch: async () =>
            {
                events.Add("dispatch:start");
                await release.Task;
                events.Add("dispatch:end");
            },
            collapse: () => events.Add("collapse"));

        // Rotation running: the progress line is painted from the count it re-wraps,
        // confirm + cancel are disabled, and the form has NOT collapsed — this is the
        // assertion the collapse-first shape fails.
        Assert.Equal(new[] { "paint 2 remaining|disabled", "dispatch:start" }, events);
        Assert.True(form.InFlight);

        release.SetResult();
        Assert.True(await confirm);

        // Only once the rotation returns: collapse, then re-enable the controls.
        Assert.Equal(
            new[]
            {
                "paint 2 remaining|disabled",
                "dispatch:start",
                "dispatch:end",
                "collapse",
                "paint |enabled",
            },
            events);
        Assert.False(form.InFlight);
    }

    [Fact]
    public async Task SecondConfirmWhileARotationRuns_IsIgnored()
    {
        var form = new MailRotateForm();
        var dispatches = 0;
        var release = new TaskCompletionSource();

        var first = form.ConfirmAsync(
            rewrapCount: 1,
            status: () => Idle,
            paint: _ => { },
            dispatch: async () => { dispatches++; await release.Task; },
            collapse: () => { });

        var second = await form.ConfirmAsync(
            rewrapCount: 1,
            status: () => Idle,
            paint: _ => { },
            dispatch: () => { dispatches++; return Task.CompletedTask; },
            collapse: () => { });

        Assert.False(second);
        Assert.Equal(1, dispatches);

        release.SetResult();
        Assert.True(await first);
        Assert.False(form.InFlight);
    }

    [Fact]
    public async Task ConfirmAfterTheFirstReturned_RunsAgain()
    {
        var form = new MailRotateForm();
        var dispatches = 0;

        for (var i = 0; i < 2; i++)
        {
            var ran = await form.ConfirmAsync(
                rewrapCount: 1,
                status: () => Idle,
                paint: _ => { },
                dispatch: () => { dispatches++; return Task.CompletedTask; },
                collapse: () => { });
            Assert.True(ran);
        }

        Assert.Equal(2, dispatches);
    }

    [Fact]
    public async Task ControlsReEnable_EvenWhenTheDispatchThrows_AndTheFormStaysOpen()
    {
        var form = new MailRotateForm();
        var events = new List<string>();

        await Assert.ThrowsAsync<InvalidOperationException>(() => form.ConfirmAsync(
            rewrapCount: 1,
            status: () => Idle,
            paint: p => events.Add(Painted(p)),
            dispatch: () => throw new InvalidOperationException("boom"),
            collapse: () => events.Add("collapse")));

        Assert.Equal(new[] { "paint 1 remaining|disabled", "paint |enabled" }, events);
        Assert.False(form.InFlight);
    }

    [Fact]
    public void Paint_IdleFormIsEmptyAndEnabled()
    {
        var p = new MailRotateForm().Paint(Idle);

        Assert.Equal(string.Empty, p.ProgressText);
        Assert.True(p.ControlsEnabled);
    }

    [Fact]
    public void Paint_SnapshotReportedRotationShowsItsRemainingCount_WithoutLockingTheControls()
    {
        // A rotation the SNAPSHOT reports (e.g. a resumed one) paints its own count;
        // only this form's own in-flight rotation disables confirm/cancel.
        var p = new MailRotateForm().Paint(new SettingsStatus.RotationInProgress(5UL));

        Assert.Equal("5 remaining", p.ProgressText);
        Assert.True(p.ControlsEnabled);
    }

    [Fact]
    public async Task Paint_InFlightPrefersTheSnapshotsOwnCountOverTheConfirmTimeOne()
    {
        var form = new MailRotateForm();
        var release = new TaskCompletionSource();
        var confirm = form.ConfirmAsync(
            rewrapCount: 3,
            status: () => Idle,
            paint: _ => { },
            dispatch: () => release.Task,
            collapse: () => { });

        Assert.Equal("3 remaining", form.Paint(Idle).ProgressText);
        Assert.Equal("2 remaining", form.Paint(new SettingsStatus.RotationInProgress(2UL)).ProgressText);
        Assert.False(form.Paint(Idle).ControlsEnabled);

        release.SetResult();
        await confirm;
    }

    [Fact]
    public void RewrapCount_SkipsRevokedAndExcludedCredentials()
    {
        // The shared Rust rule (state.rs `rotation_rewrap_count`) through the REAL
        // UniFFI export — windows no longer re-derives it (android had to, before it
        // was exported).
        var snap = Snapshot(Cred("default"), Cred("phone"), Cred("burned", revoked: true));

        Assert.Equal(2UL, FaunaClientMailSettingsMethods.RotationRewrapCount(snap, Array.Empty<string>()));
        Assert.Equal(1UL, FaunaClientMailSettingsMethods.RotationRewrapCount(snap, new[] { "phone" }));
        Assert.Equal(0UL, FaunaClientMailSettingsMethods.RotationRewrapCount(Snapshot(), Array.Empty<string>()));
    }
}
