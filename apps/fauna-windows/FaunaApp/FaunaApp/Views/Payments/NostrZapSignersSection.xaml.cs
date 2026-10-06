using System;
using System.Linq;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Payments;

/// <summary>
/// The *Zap signers* surface of the standalone Nostr page (monetization.md §
/// Zap receipts — the trust model; nostr.md § Layout &amp; flow item 7), lifted
/// out of <see cref="NostrPage"/> whole so the whole plane is one removable
/// build item — the same shape <see cref="PaymentsAuthorSections"/> takes one
/// page over.
/// </summary>
/// <remarks>
/// A dumb renderer over <see cref="NostrViewModel"/>, exactly as the page it
/// came from: the page still owns the view model, hydration and every other
/// section, and drives this control through <see cref="Attach"/> +
/// <see cref="Render"/>. <see cref="NostrPage"/> re-renders imperatively after
/// every mutation (it is not <c>PropertyChanged</c>-driven), so this control's
/// own handlers call back into the host's re-render via the delegate
/// <see cref="Attach"/> takes, rather than only updating themselves — the
/// shared <c>error-message</c> surface lives on the host page.
/// </remarks>
public sealed partial class NostrZapSignersSection : UserControl
{
    private NostrViewModel? _vm;
    private Action? _onMutated;

    public NostrZapSignersSection()
    {
        InitializeComponent();
    }

    /// <summary>Bind to the page's view model and its re-render callback.
    /// Called once, from the host page's <c>Page_Loaded</c>, before its first
    /// <see cref="Render"/>.</summary>
    public void Attach(NostrViewModel vm, Action onMutated)
    {
        _vm = vm;
        _onMutated = onMutated;
    }

    /// <summary>Paint the section from the current VM state — called from the
    /// host page's <c>RenderState</c> after every load/mutation.</summary>
    public void Render()
    {
        if (_vm is null) return;
        ZapSignersList.ItemsSource = _vm.ZapSigners.ToList();
        ZapSignersEmpty.Visibility =
            _vm.ZapSigners.Count == 0 ? Visibility.Visible : Visibility.Collapsed;

        // Never disabled eagerly: an un-hydrated read leaves ZapSignerAddGateReason
        // null, which leaves this button live (the nest, not the app, is the
        // enforcement floor).
        var reason = _vm.ZapSignerAddGateReason;
        AddZapSignerButton.IsEnabled = reason is null;
        ZapSignerGateReasonText.Text = reason ?? string.Empty;
        ZapSignerGateReasonText.Visibility =
            string.IsNullOrEmpty(reason) ? Visibility.Collapsed : Visibility.Visible;
    }

    private async void AddZapSigner_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null) return;
        await _vm.AddZapSignerAsync(ZapSignerPubkeyInput.Text, ZapSignerLabelInput.Text);
        if (string.IsNullOrEmpty(_vm.ErrorMessage))
        {
            ZapSignerPubkeyInput.Text = string.Empty;
            ZapSignerLabelInput.Text = string.Empty;
        }
        _onMutated?.Invoke();
    }

    private async void RemoveZapSigner_Click(object sender, RoutedEventArgs e)
    {
        if (_vm is null || sender is not Button btn || btn.Tag is not string pubkey) return;
        // Resolve by value, not by a captured index: the roster re-projects on
        // every refresh, so a stale index could remove the wrong row.
        var index = _vm.ZapSigners.ToList().FindIndex(s => s.SignerPubkey == pubkey);
        await _vm.RemoveZapSignerAsync(index);
        _onMutated?.Invoke();
    }
}
