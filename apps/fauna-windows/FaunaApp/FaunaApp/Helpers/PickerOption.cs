using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;

namespace FaunaApp.Helpers;

/// <summary>
/// The one way a windows picker builds an option whose stable key differs from the
/// label it paints (a wire value, a token, a row id): the windows face of
/// <c>docs/goal/architecture/e2e-conventions.md</c> convention 11, <i>What counts as
/// "offered"</i> — the suite drives keys while every app paints labels.
///
/// <para>A WinUI <see cref="ComboBoxItem"/> carries the two on separate properties.
/// Its <c>Content</c> and UIA Name are the painted label: what a screen reader
/// announces, and what the FlaUI bridge's <c>options</c> attribute lists, so the
/// cross-app <c>option_texts</c> answers painted texts on windows as on every other
/// app. The key is its AutomationId and, unless the caller tags the item with
/// something richer, its <c>Tag</c> — what the page dispatches. The bridge's
/// <c>select</c> matches an item's AutomationId before its Name, and its
/// <c>get_text</c> on a ComboBox answers the selected item's AutomationId when it
/// has one, so <c>driver.select(id, key)</c> and the key read-back work unchanged.</para>
///
/// <para>A picker whose label IS its identity (a tier name, an account handle)
/// needs no key and no helper: a string <c>Content</c> names the item by itself.
/// <c>test_windows_picker_option_ratchet.py</c> refuses a hand-set Name that is not
/// the painted label.</para>
/// </summary>
internal static class PickerOption
{
    /// <summary>An option painting <paramref name="label"/> and keyed by
    /// <paramref name="key"/>. <paramref name="tag"/> defaults to the key; an empty
    /// key (a "none" or "every domain" option) sets no AutomationId, so the item
    /// reads back by its label.</summary>
    public static ComboBoxItem Create(string label, string key, object? tag = null)
    {
        var item = new ComboBoxItem { Content = label, Tag = tag ?? key };
        AutomationProperties.SetName(item, label);
        if (key.Length > 0) AutomationProperties.SetAutomationId(item, key);
        return item;
    }
}
