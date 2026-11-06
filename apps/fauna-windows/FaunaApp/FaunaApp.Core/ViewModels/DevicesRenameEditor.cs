using System.Collections.Generic;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The Settings → Devices page's device-name editor (ui/devices.md § Device names;
/// IDs user-approved 2026-11-01). View state only: WHICH device's editor is open
/// and the text typed so far. Everything else — the prefill
/// (<c>DevicesMachine.DeviceName</c>), the seal, the unchanged/blank/reserved-name
/// refusals — is the shared machine's (<c>RenameDevice</c>).
///
/// <para>Keyed by <c>device_id</c>, never the painted index: a roster refresh while
/// the user types re-sorts or inserts rows, and the editor must follow its device
/// (the same rule the member-remove arm follows). One editor is open at a time —
/// opening another replaces it. Confirm and cancel both close it.</para>
/// </summary>
public sealed class DevicesRenameEditor
{
    /// <summary>The device whose editor is open, or <c>null</c>.</summary>
    public string? DeviceId { get; private set; }

    /// <summary>The text in <c>device-rename-input</c> (seeded by <see cref="Open"/>).</summary>
    public string Draft { get; private set; } = "";

    public bool IsOpen => DeviceId is not null;

    public bool IsOpenFor(string deviceId) => DeviceId == deviceId;

    /// <summary>Open the editor over <paramref name="deviceId"/>, prefilled with
    /// <paramref name="prefill"/> (the machine's <c>DeviceName</c>, or empty when
    /// it knows none). Replaces any editor already open.</summary>
    public void Open(string deviceId, string? prefill)
    {
        DeviceId = deviceId;
        Draft = prefill ?? "";
    }

    public void SetDraft(string text)
    {
        if (IsOpen) Draft = text;
    }

    /// <summary>Close without writing anything (cancel, and the close after a confirm).</summary>
    public void Close()
    {
        DeviceId = null;
        Draft = "";
    }

    /// <summary>The rename elements one <c>device-card</c> paints, in order:
    /// always the indexed <c>device-rename-button</c>; with the editor open over
    /// that card, then the input, confirm and cancel (the same sequence tui's
    /// card pins). The page's XAML paints exactly these ids.</summary>
    public static IReadOnlyList<string> CardElementIds(bool editorOpenOnThisCard)
        => editorOpenOnThisCard
            ? new[]
            {
                "device-rename-button",
                "device-rename-input",
                "device-rename-confirm-button",
                "device-rename-cancel-button",
            }
            : new[] { "device-rename-button" };
}
