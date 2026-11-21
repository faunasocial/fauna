using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// The device outbox's three unsent-changes doors (<c>account-offline-mutation.md</c>
/// § The offline-mutation contract → <i>The unsent-changes surface</i>, points 1
/// and 4): list every change this device holds that has not reached the nest, retry a
/// not-sent one, discard a not-sent one. They read and write the device's own outbox,
/// never the nest, so they answer with the nest down. A seam only so a unit test can
/// stand in for the process outbox; <see cref="DeviceUnsentChangesDoors"/> is the one
/// production implementation (android calls the same three UniFFI exports).
/// </summary>
internal interface IUnsentChangesDoors
{
    /// <summary>Every unsent change, scope order then compose order (<c>unsent_changes</c>).</summary>
    Task<IReadOnlyList<UnsentChange>> ListAsync();

    /// <summary>Re-arm a not-sent change (<c>retry_unsent_change</c>); <c>false</c> = no such
    /// not-sent change.</summary>
    Task<bool> RetryAsync(string intentId);

    /// <summary>Delete a not-sent change, behind the surface's confirm
    /// (<c>discard_unsent_change</c>); <c>false</c> = no such not-sent change.</summary>
    Task<bool> DiscardAsync(string intentId);
}

/// <summary>The process outbox's doors, over the UniFFI exports.</summary>
internal sealed class DeviceUnsentChangesDoors : IUnsentChangesDoors
{
    public async Task<IReadOnlyList<UnsentChange>> ListAsync() => await FaunaFfiMethods.UnsentChanges();

    public async Task<bool> RetryAsync(string intentId) => await FaunaFfiMethods.RetryUnsentChange(intentId);

    public async Task<bool> DiscardAsync(string intentId) => await FaunaFfiMethods.DiscardUnsentChange(intentId);
}

/// <summary>
/// One unsent change in words — the shared describer's label and the shared state and
/// reason text (<c>unsent_change_state_text</c> / <c>unsent_change_reason_text</c>),
/// resolved here and composed nowhere else (android's <c>unsentDescriber</c> /
/// <c>unsentStatusLine</c>, FaunaKit's twin).
/// </summary>
internal static class UnsentChangeText
{
    /// <summary><c>unsent-change-description</c>: the kind and target in user words; an
    /// unregistered kind's label is its kind string (the describer's own fallback), and a
    /// label that resolves to nothing falls back to the kind, never a blank row.</summary>
    internal static string Describe(UnsentChange change)
    {
        var label = Strings.Resolve(change.@description.@label);
        return string.IsNullOrEmpty(label) ? change.@kind : label;
    }

    /// <summary><c>unsent-change-status</c>: the state, and on a row that says why it was not
    /// sent, the reason through <c>settings.unsent_changes.status_with_reason</c>.</summary>
    internal static string Status(UnsentChange change)
    {
        var state = Strings.Resolve(FaunaFfiMethods.UnsentChangeStateText(change));
        if (FaunaFfiMethods.UnsentChangeReasonText(change) is not { } reasonText) return state;
        return Strings.Resolve(new LocalizedText(
            "settings.unsent_changes.status_with_reason",
            new Dictionary<string, string>
            {
                ["state"] = state,
                ["reason"] = Strings.Resolve(reasonText),
            }));
    }
}
