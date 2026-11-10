using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using uniffi.fauna_core;

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
/// <para>A picker whose label IS its identity (a tier name, a provider kind)
/// needs no key: a string <c>Content</c> names the item by itself, and
/// <see cref="Reconcile"/>'s label-only form builds it with none. <c>test_windows_picker_option_ratchet.py</c> refuses a hand-set Name
/// that is not the painted label, and an <c>Items.Clear()</c> anywhere but
/// here.</para>
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

    /// <summary>Point <paramref name="combo"/> at <paramref name="options"/> (each a
    /// painted label and its key, built by <see cref="Create"/>) and select the one
    /// keyed <paramref name="selectedKey"/>, REBUILDING THE ITEMS ONLY WHEN THE OPTION
    /// SET ITSELF CHANGED: the one way a windows picker repaints from its view model.
    /// The other forms below differ only in how an option is spelled; all of them
    /// come here, and nothing else in the app clears a picker's items
    /// (<c>test_windows_picker_option_ratchet.py</c>).
    /// <para>The idempotence is load-bearing, not an optimisation. A picker painted
    /// from a view model is repainted after its own gesture: the selection's dispatch
    /// re-renders, and its <c>Selected…</c> property change repaints the picker
    /// (synchronously inside the <c>SelectionChanged</c>, or a moment later when the
    /// dispatch awaits the nest). An unconditional <c>Items.Clear()</c> there destroys
    /// every <see cref="ComboBoxItem"/>, the one just picked included, while the
    /// popup the automation client expanded is still closing and that client still
    /// holds providers on the items. XAML answers from native code with a stowed
    /// <c>E_UNEXPECTED</c> ("Catastrophic failure"), no managed frame of ours on the
    /// stack, and the process dies <c>0xC000027B</c>. Measured on the Backups folder
    /// picker, four rebuilds 9 ms apart inside one <c>SelectionChanged</c>,
    /// and on the mail-reports domain filter, one death in two runs after a FlaUI
    /// <c>select</c>. A race, so a lucky timing passes and
    /// proves nothing.</para>
    /// <para>The selection is reconciled separately from the option set: it changes
    /// far more often, and moving it needs no teardown at all. Nothing is assigned
    /// when the wanted item is already selected; no item keyed
    /// <paramref name="selectedKey"/> selects nothing, which is what a rebuild used to
    /// leave behind. Both a rebuild and a moved selection raise
    /// <c>SelectionChanged</c>, so the caller holds its own re-entry guard around
    /// this call, or the repaint reads as a gesture.</para>
    /// <para><paramref name="firstIfAbsent"/> selects the first option instead when
    /// none is keyed <paramref name="selectedKey"/>: a picker that must always show an
    /// answer (a tier, a role) rather than a blank.</para></summary>
    public static void Reconcile(ComboBox combo, IReadOnlyList<(string Label, string Key)> options,
        string? selectedKey, bool firstIfAbsent = false)
        => Reconcile(combo, options.Select(o => (o.Label, o.Key, o.Key)).ToList(), selectedKey, firstIfAbsent);

    /// <summary><see cref="Reconcile"/>'s label-only form, for a picker whose label IS
    /// its identity (a tier name, a provider kind, a
    /// bridge id): each item carries no key, reads back by its label, and dispatches
    /// its label (its <c>Tag</c>).</summary>
    public static void Reconcile(ComboBox combo, IReadOnlyList<string> labels, string? selectedLabel,
        bool firstIfAbsent = false)
        => Reconcile(combo, labels.Select(l => (l, "", l)).ToList(), selectedLabel, firstIfAbsent);

    /// <summary>The general form the others spell: each option is its painted
    /// <c>Label</c>, its <c>Key</c> (the AutomationId the suite drives and reads
    /// back; empty for none, when the item reads back by its label) and the
    /// <c>Value</c> the page dispatches (its <c>Tag</c>), and the option whose value is
    /// <paramref name="selectedValue"/> is selected. For a picker the suite drives
    /// by label while the page dispatches something else (a wire value, an actor
    /// id).</summary>
    public static void Reconcile(ComboBox combo,
        IReadOnlyList<(string Label, string Key, string Value)> options, string? selectedValue,
        bool firstIfAbsent = false)
    {
        ReconcileItems(combo, options);
        Select(combo, selectedValue, firstIfAbsent);
    }

    /// <summary><see cref="Reconcile"/>'s selection half alone: move
    /// <paramref name="combo"/>'s selection to the item dispatching
    /// <paramref name="value"/> among the items it offers NOW, never touching them (the
    /// first item when <paramref name="firstIfAbsent"/> and none dispatches it, else
    /// nothing). For the echo of a picker's own gesture whose option list lives in a
    /// view-model collection that can move on without repainting the picker (a tier
    /// list, the guardian candidates): a full <see cref="Reconcile"/> there would rebuild
    /// inside the gesture if the collection had moved, which is the very teardown it
    /// exists to prevent. The picker is repainted from the collection by its own paint
    /// path.</summary>
    public static void Select(ComboBox combo, string? value, bool firstIfAbsent = false)
    {
        ComboBoxItem? first = null;
        foreach (var candidate in combo.Items)
        {
            if (candidate is not ComboBoxItem item) continue;
            first ??= item;
            if (item.Tag is string tag && tag == value)
            {
                if (!ReferenceEquals(combo.SelectedItem, item)) combo.SelectedItem = item;
                return;
            }
        }
        var fallback = firstIfAbsent ? first : null;
        if (!ReferenceEquals(combo.SelectedItem, fallback)) combo.SelectedItem = fallback;
    }

    /// <summary>The positional form, for a label-only picker whose answer is
    /// resolved by POSITION because two of its options may paint the same text (the
    /// composer's audience select, where a tier can be named like a room option):
    /// the items as <see cref="Reconcile"/>'s label-only form builds them, then the
    /// item at <paramref name="selectedIndex"/> selected.</summary>
    public static void ReconcileAtIndex(ComboBox combo, IReadOnlyList<string> labels, int selectedIndex)
    {
        ReconcileItems(combo, labels.Select(l => (l, "", l)).ToList());
        if (combo.SelectedIndex != selectedIndex) combo.SelectedIndex = selectedIndex;
    }

    /// <summary>A picker over a shared-Rust <see cref="ReachPolicyOption"/> catalog
    /// (a policy knob's allow / hold / reject and its siblings), selecting
    /// <paramref name="selected"/> by VALUE, never by position. A value this build's
    /// catalog does not list (a stored token from a newer build) selects
    /// <paramref name="failClosed"/>: each knob's own established default, not
    /// necessarily its strictest option, mirroring the shared
    /// <c>from_wire_or_default</c>. Each item paints the localized label and
    /// dispatches the wire value; <paramref name="keyed"/> says whether the wire value
    /// is also its key (the suite drives the admin mail knobs by value) or the item
    /// carries none and reads back by its label (the suite drives the family knobs
    /// by label).</summary>
    public static void ReconcileCatalog(ComboBox combo, IReadOnlyList<ReachPolicyOption> catalog,
        string selected, string failClosed, bool keyed)
    {
        var options = catalog
            .Select(o => (Strings.Resolve(o.@label), keyed ? o.@value : "", o.@value))
            .ToList();
        var value = options.Any(o => o.Item3 == selected) ? selected : failClosed;
        Reconcile(combo, options, value);
    }

    /// <summary>Make <paramref name="combo"/> offer exactly <paramref name="options"/>,
    /// tearing its items down only when they differ: the one <c>Items.Clear()</c> of
    /// the windows app.</summary>
    private static void ReconcileItems(ComboBox combo,
        IReadOnlyList<(string Label, string Key, string Value)> options)
    {
        if (Matches(combo, options)) return;
        combo.Items.Clear();
        foreach (var (label, key, value) in options) combo.Items.Add(Create(label, key, value));
    }

    /// <summary>True when <paramref name="combo"/> already offers exactly
    /// <paramref name="options"/>, in order: the predicate that decides whether a
    /// teardown is owed at all.</summary>
    private static bool Matches(ComboBox combo,
        IReadOnlyList<(string Label, string Key, string Value)> options)
    {
        if (combo.Items.Count != options.Count) return false;
        for (int i = 0; i < options.Count; i++)
        {
            if (combo.Items[i] is not ComboBoxItem { Tag: string tag, Content: string label } item
                || tag != options[i].Value || label != options[i].Label
                || AutomationProperties.GetAutomationId(item) != options[i].Key)
                return false;
        }
        return true;
    }
}
