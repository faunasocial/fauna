using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using uniffi.fauna_devices_machine;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings "Task delegation" sub-page VM (participants.md § Task delegation
/// + § The assignment picker; placement ratified 2026-07-08, settings.md §
/// Navigation model — after Nests). Dumb rendering of the shared
/// <c>fauna_client_delegation::TaskDelegationView</c> (via
/// <c>FfiNestClient.TaskDelegationViewForDevice</c>) — this layer holds NO delegation
/// policy (priority #2). The option list a picker offers is a correctness
/// surface the shared layer guarantees (a pin to a target that can never run
/// the kind would strand it forever), so <see cref="TaskDelegationRowVm.PinOptions"/>
/// is always rendered verbatim, never constructed/filtered here.
///
/// windows is a native desktop that runs the content-index builder but ships no
/// segment-backup upload driver, so it passes
/// <see cref="FfiHeavyTaskCapability.IndexOnly"/> (2026-08-16, the slice-5 flip):
/// the picker offers "This device" for <c>index</c> and withholds it for
/// <c>backup-upload</c>, which the source nest writes.
/// Reference render: <c>apps/fauna-linux/src/settings/task_delegation.rs</c>.
///
/// Participant display names (for a runner / pin that is another device) are
/// resolved from the shared <c>DevicesMachine</c> roster (device_id hex →
/// label), since device names are inherently client-side state the shared
/// view-model deliberately does not bake in (<c>RunnerStatus::Other</c> /
/// <c>PinOption::Other</c>). The runner/option display TEXT itself is the
/// shared <c>fauna_core::delegation::{runner_label,option_label}</c> decision
/// over UniFFI (<c>FaunaFfiMethods.TaskDelegation{Runner,Option}Label</c>) —
/// this layer supplies only the roster, never hand-rolls the label (priority
/// #2; mirrors apple's <c>runnerLabel</c>/<c>optionLabel</c> free functions).
/// </summary>
public partial class TaskDelegationViewModel : ViewModelBase
{
    private readonly INestRpcClient _nest;
    private readonly string _deviceId;

    internal ObservableCollection<TaskDelegationRowVm> Rows { get; } = new();

    /// <summary>device_id hex → display label, from the shared device roster —
    /// used to name a runner / pinned participant that isn't this device.</summary>
    public Dictionary<string, string> Labels { get; private set; } = new();

    internal TaskDelegationViewModel(INestRpcClient nest, string deviceId)
    {
        _nest = nest;
        _deviceId = deviceId;
    }

    /// <summary>Load the surface: the per-kind rows plus the device-label map.
    /// Call every time the page becomes visible — the runner column is *live*
    /// advisory-lease state (mirrors linux's page.connect_map re-load).</summary>
    public async Task LoadAsync()
    {
        ErrorMessage = null;
        try
        {
            Labels = await DeviceLabelsAsync();
            var rows = await _nest.TaskDelegationListAsync(_deviceId);
            Rows.Clear();
            foreach (var row in rows) Rows.Add(TaskDelegationRowVm.From(row, Labels));
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>Persist a pin change for <paramref name="taskKind"/>, then reload
    /// so the runner column + the picker reflect authoritative state (a CAS
    /// read-modify-write — a failure changed nothing).</summary>
    internal async Task SetAssignmentAsync(string taskKind, FfiPinOption option)
    {
        ErrorMessage = null;
        try
        {
            await _nest.TaskDelegationSetAssignmentAsync(_deviceId, taskKind, option);
            await LoadAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
    }

    /// <summary>The device_id→label map from the shared <c>DevicesMachine</c>
    /// roster. Best-effort: a read failure yields an empty map, and the shared
    /// label functions then fall back to a short-hex abbreviation rather than
    /// showing nothing.</summary>
    private async Task<Dictionary<string, string>> DeviceLabelsAsync()
    {
        try
        {
            var machine = await _nest.BuildDevicesMachineAsync(new NoopDevicesObserver());
            await machine.Refresh();
            return machine.Snapshot().devices.ToDictionary(d => d.deviceId, d => d.label);
        }
        catch
        {
            return new Dictionary<string, string>();
        }
    }

    /// <summary>No-op observer: this page reads the roster once per load (for
    /// display names) rather than reacting to it, so it needs no reactivity
    /// (mirrors linux's <c>NoopObserver</c>).</summary>
    private sealed class NoopDevicesObserver : DevicesObserver
    {
        public void OnChanged()
        {
        }
    }
}

/// <summary>One <c>task-delegation-kind-item</c> row: the kind's resolved
/// display name, the current runner line, and the picker's current assignment
/// + legal options (rendered verbatim).</summary>
internal sealed class TaskDelegationRowVm
{
    public required string TaskKind { get; init; }
    public required string Name { get; init; }
    public required string RunnerText { get; init; }
    public required FfiPinOption Assignment { get; init; }
    public required IReadOnlyList<FfiPinOption> PinOptions { get; init; }

    internal static TaskDelegationRowVm From(FfiTaskDelegationRow row, Dictionary<string, string> labels) =>
        new()
        {
            TaskKind = row.taskKind,
            Name = Strings.Resolve(row.name),
            RunnerText = ResolveRunnerText(row.runner, labels),
            Assignment = row.assignment,
            PinOptions = row.pinOptions,
        };

    /// <summary>Label the current-runner line via the shared
    /// <c>fauna_core::delegation::runner_label</c> decision over UniFFI
    /// (participant-name resolution included) — never hand-rolled here.
    /// `FfiException` is unreachable for a well-formed row (a malformed
    /// 32-byte nest pubkey); the empty-string fallback matches apple's
    /// <c>runnerLabel</c>.</summary>
    private static string ResolveRunnerText(FfiRunnerStatus runner, Dictionary<string, string> labels)
    {
        try
        {
            return Strings.Resolve(FaunaFfiMethods.TaskDelegationRunnerLabel(runner, labels));
        }
        catch (FfiException)
        {
            return string.Empty;
        }
    }

    /// <summary>The picker's visible Content — the localized label the user
    /// reads, via the shared <c>fauna_core::delegation::option_label</c>
    /// decision over UniFFI (mirrors <see cref="ResolveRunnerText"/>). NEVER
    /// the stable key <see cref="OptionKey"/> uses for the ComboBoxItem's
    /// automation Name.</summary>
    internal static string OptionLabel(FfiPinOption option, Dictionary<string, string> labels)
    {
        try
        {
            return Strings.Resolve(FaunaFfiMethods.TaskDelegationOptionLabel(option, labels));
        }
        catch (FfiException)
        {
            return string.Empty;
        }
    }

    /// <summary>The stable cross-app option key ("automatic" / "this-device" /
    /// a participant-ref hex for a foreign pin — never offered by the picker,
    /// only rendered as an already-pinned-elsewhere row's current assignment).
    /// FlaUI's <c>Select()</c> matches a ComboBoxItem's Name to this EXACTLY, with
    /// no normalization (reference_windows_flaui_select_exact_name) — so this
    /// must back <c>AutomationProperties.Name</c>, never the visible Content.</summary>
    internal static string OptionKey(FfiPinOption option) =>
        option switch
        {
            FfiPinOption.Automatic => "automatic",
            FfiPinOption.ThisDevice => "this-device",
            FfiPinOption.Other other => ParticipantKeyHex(other.@who),
            _ => string.Empty,
        };

    /// <summary>The stable cross-app hex key for a participant ref — the
    /// <see cref="OptionKey"/> encoding, distinct from the display name (which
    /// the shared <c>option_label</c>/<c>runner_label</c> functions resolve
    /// internally from the same roster, via <see cref="ResolveRunnerText"/> /
    /// <see cref="OptionLabel"/>).</summary>
    private static string ParticipantKeyHex(FfiParticipantRef who) =>
        who switch
        {
            FfiParticipantRef.Device device => device.@deviceId,
            FfiParticipantRef.Nest nest => FaunaFfiMethods.HexFull(nest.@actorPubkey),
            _ => string.Empty,
        };
}
