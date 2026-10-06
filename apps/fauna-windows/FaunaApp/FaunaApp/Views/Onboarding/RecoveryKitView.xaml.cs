using System;
using System.ComponentModel;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// The recovery-kit offer (<c>onboarding.md</c> § 1 Identity), right after the
/// identity secret is confirmed on the CREATE path — windows' leg of the screen
/// tui led. Mirrors
/// <c>apps/fauna-linux/src/views/onboarding/recovery_kit.rs</c>.
///
/// <para>The page <b>mints and displays only</b>: no nest exists at this
/// position, so registration + escrow are queued at the wizard's LoggedIn terminal
/// (<see cref="OnboardingViewModel"/>'s <c>QueueDeferredRecoveryKitRegistration</c>)
/// and run on the signed-in session's client.
/// The display is the bare 64-hex; the QR and the copy button both carry the
/// machine's one <c>fauna://recovery</c> URI (<c>identity-succession.md</c>
/// § The RecoveryKey → <i>Which encoding each affordance carries</i>).</para>
/// </summary>
public sealed partial class RecoveryKitView : UserControl
{
    private readonly OnboardingViewModel? _vm;
    // The URI the canvas currently shows — repainting ~1000 Rectangles on every
    // observer tick would be wasteful, and the URI changes only when a kit is
    // minted or dropped.
    private string? _paintedUri;

    public RecoveryKitView()
    {
        InitializeComponent();
    }

    internal RecoveryKitView(OnboardingViewModel vm) : this()
    {
        _vm = vm;
        DataContext = vm;
        vm.PropertyChanged += OnViewModelPropertyChanged;
        RepaintQr();
    }

    private void OnViewModelPropertyChanged(object? sender, PropertyChangedEventArgs e) => RepaintQr();

    private void RepaintQr()
    {
        var uri = _vm?.RecoveryKitUri;
        if (uri == _paintedUri) return;
        _paintedUri = uri;
        if (uri is null)
        {
            FaunaApp.Helpers.QrPainter.Clear(QrCanvas);
            return;
        }
        try
        {
            // Over the SHARED fauna_core qr grid — never a platform QR library.
            FaunaApp.Helpers.QrPainter.Paint(QrCanvas, uri);
        }
        catch (Exception ex)
        {
            // The QR is the convenience; the 64-hex above it is the kit. A QR that
            // could not be drawn must never take the secret off screen with it.
            FaunaApp.Core.Logs.ShellLog.Error("RecoveryKitView", $"recovery kit QR paint failed: {ex.Message}");
            FaunaApp.Helpers.QrPainter.Clear(QrCanvas);
        }
    }

    // Read at click time, never cached: the URI lives exactly as long as the
    // machine holds the pending root.
    private void CopyButton_Click(object sender, RoutedEventArgs e)
    {
        if (_vm?.RecoveryKitUri is { } uri)
            FaunaApp.Helpers.ClipboardHelper.CopyText(uri);
    }
}
