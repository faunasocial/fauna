using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Controls;

/// <summary>
/// The <c>member-review-row</c> family's one renderer — one row per person, with
/// <c>member-review-keep-button</c> / <c>member-review-remove-button</c> scoped
/// inside it — shared by the permanent Members To Review page and the ephemeral
/// kit-side pass (<c>docs/goal/ui/settings.md</c> § Recovery kit → <i>The
/// ephemeral member-review pass</i>: one id family, two surfaces, the row built
/// once). A pure renderer: the host decides what to show and what a press means.
/// </summary>
public sealed partial class MemberReviewList : UserControl
{
    public MemberReviewList()
    {
        InitializeComponent();
    }

    /// <summary>Keep pressed on the row naming this person (the raw 32-byte actor
    /// id — addressed by the PERSON, never the painted index, since the roster
    /// re-orders on every re-read).</summary>
    public event Action<byte[]>? KeepRequested;

    /// <summary>Remove pressed on the row naming this person.</summary>
    public event Action<byte[]>? RemoveRequested;

    /// <summary>Paint exactly these rows. An empty list removes every row from the
    /// tree rather than hiding it, so a driver counting <c>member-review-row</c>
    /// reads 0 the moment a host's gate closes.</summary>
    public void Show(IReadOnlyList<MemberReviewRow> rows) => Rows.ItemsSource = rows;

    private void Keep_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: byte[] person }) KeepRequested?.Invoke(person);
    }

    private void Remove_Click(object sender, RoutedEventArgs e)
    {
        if (sender is Button { Tag: byte[] person }) RemoveRequested?.Invoke(person);
    }
}
