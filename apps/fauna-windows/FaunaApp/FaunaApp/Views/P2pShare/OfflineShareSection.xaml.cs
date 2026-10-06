using System;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Services;
using uniffi.fauna_client_capabilities;
using uniffi.fauna_ffi;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views.P2pShare;

/// <summary>
/// The offline co-present share section of the Folders page (p2p.md § Offline
/// share initiation), lifted out of <see cref="FoldersPage"/> whole so the
/// ceremony's painted surface is one removable build item — the same shape
/// <c>Views.Payments.NostrZapSignersSection</c> takes for <c>payments</c>.
/// </summary>
/// <remarks>
/// A nest-free, iroh-direct two-party ceremony. Seat/panel/status/expecting-
/// from live on App (<c>App.CurrentOfflineShare*</c>), not on this control or
/// its page, because the page has no NavigationCacheMode (a fresh instance per
/// navigation) and the recipient's own "did the invitation land" wait
/// re-navigates repeatedly (<c>test_offline_share_two_seat.py::_consent_card_count</c>)
/// — a page-scoped seat would have its armed ExpectFrom listener destroyed by
/// that re-navigation. Painting is pure: <c>FaunaFfiMethods.OfflineShareView</c>/
/// <c>ParsePeerCode</c>/<c>StatusLabel</c>/<c>CodeErrorLabel</c> are all local,
/// synchronous, no-nest-round-trip reads — only the seat bind + the ceremony
/// acts below are async.
///
/// <para>The host page keeps the knock list, the folder machine and the shared
/// <c>error-message</c> surface: <see cref="Attach"/> hands this control a
/// callback for "a ceremony act resolved" (the host re-fetches the knock list
/// and, when the status lands a scope, refreshes the machine) and one for
/// "report this error".</para>
///
/// <para>⚠ No <c>ConfigureAwait(false)</c> — WinUI rule, as on the host page.</para>
/// </remarks>
public sealed partial class OfflineShareSection : UserControl
{
    private INestRpcClient? _rpc;
    private ICryptoService? _crypto;
    private Func<CeremonyStatus, Task>? _onActResolved;
    private Action<string>? _reportError;

    public OfflineShareSection()
    {
        InitializeComponent();
    }

    /// <summary>Bind to the host page's seams. Called from the host's
    /// <c>Page_Loaded</c>, before its first <see cref="Render"/>.</summary>
    internal void Attach(
        INestRpcClient rpc,
        ICryptoService? crypto,
        Func<CeremonyStatus, Task> onActResolved,
        Action<string> reportError)
    {
        _rpc = rpc;
        _crypto = crypto;
        _onActResolved = onActResolved;
        _reportError = reportError;
    }

    private void ReportError(Exception ex) =>
        _reportError?.Invoke(S.Format("folders/error_offline_share", ex.Message));

    /// <summary>The single paint point for the whole section — entry buttons,
    /// own code, peer-code hint, status text, begin/expect gating. Fail-safe
    /// COLLAPSED: no identity secret, or the pure FFI read itself throwing,
    /// hides the whole section rather than breaking the rest of the page.</summary>
    public void Render()
    {
        if (_crypto is null) { OfflineShareRoot.Visibility = Visibility.Collapsed; return; }

        var panel = App.CurrentOfflineSharePanel;
        var status = App.CurrentOfflineShareStatus;
        var seat = App.CurrentOfflineShareSeat;
        var peerCodeInput = OfflineSharePeerCodeInputBox.Text;

        OfflineShareView view;
        PeerCodeParsed parsed;
        OfflineShareGates gates;
        try
        {
            view = FaunaFfiMethods.OfflineShareView(panel, _crypto.SecretBytes, seat, peerCodeInput, status);
            parsed = FaunaFfiMethods.OfflineShareParsePeerCode(peerCodeInput, _crypto.SecretBytes);
            gates = FaunaFfiMethods.OfflineShareGates(view);
        }
        catch
        {
            OfflineShareRoot.Visibility = Visibility.Collapsed;
            return;
        }

        OfflineShareRoot.Visibility = view.available ? Visibility.Visible : Visibility.Collapsed;
        if (!view.available) return;

        OfflineShareEntryContainer.Visibility = gates.showsEntryButtons ? Visibility.Visible : Visibility.Collapsed;
        OfflineShareOpenContainer.Visibility = gates.showsCodeWidgets ? Visibility.Visible : Visibility.Collapsed;
        if (!gates.showsCodeWidgets) return;

        OfflineShareOwnCodeBox.Text = view.ownCode;
        OfflineShareStatusText.Text = S.Resolve(FaunaFfiMethods.OfflineShareStatusLabel(status));

        var errorLabel = parsed.error is { } err ? FaunaFfiMethods.OfflineShareCodeErrorLabel(err) : null;
        OfflineShareCodeHintText.Text = errorLabel is { } el ? S.Resolve(el) : string.Empty;
        OfflineShareCodeHintText.Visibility = errorLabel is not null ? Visibility.Visible : Visibility.Collapsed;

        // Every act gate below is the shared fauna_ffi::offline_share_gates
        // door's own answer, never a C#-side re-derivation of can_begin /
        // can_expect / shows_cancel; mirrors
        // android's DevicesVM and apple's OfflineShareSectionView.
        OfflineShareBeginButtonEl.Visibility = panel == OfflineSharePanel.Initiate ? Visibility.Visible : Visibility.Collapsed;
        OfflineShareBeginButtonEl.IsEnabled = gates.canBegin;
        OfflineReceiveExpectButtonEl.Visibility = panel == OfflineSharePanel.Receive ? Visibility.Visible : Visibility.Collapsed;
        OfflineReceiveExpectButtonEl.IsEnabled = gates.canExpect;
        OfflineShareCancelButtonEl.Visibility = gates.showsCancel ? Visibility.Visible : Visibility.Collapsed;
    }

    private async void OfflineShareButton_Click(object sender, RoutedEventArgs e) =>
        await OpenPanelAsync(OfflineSharePanel.Initiate);

    private async void OfflineReceiveButton_Click(object sender, RoutedEventArgs e) =>
        await OpenPanelAsync(OfflineSharePanel.Receive);

    /// <summary>Bind the seat when the panel OPENS, not at login (p2p.md §
    /// Offline share initiation → *Built — the affordance, both roles*): an
    /// actor-keyed endpoint for a feature most people never touch would be
    /// waste, and opening a panel is the co-present user's explicit "I am
    /// doing this now". Reused if already bound — one actor-keyed endpoint per
    /// session. The panel is flipped to open only AFTER a fresh bind resolves
    /// (never optimistically before), so own-code is never shown blank.</summary>
    private async Task OpenPanelAsync(OfflineSharePanel panel)
    {
        if (_rpc is null) return;
        if (App.CurrentOfflineShareSeat is null)
        {
            try { App.CurrentOfflineShareSeat = await _rpc.BindOfflineShareSeatAsync(); }
            catch (Exception ex)
            {
                ReportError(ex);
                return;
            }
        }
        App.CurrentOfflineSharePanel = panel;
        Render();
    }

    private void OfflineSharePeerCodeInput_Changed(object sender, TextChangedEventArgs e) => Render();

    /// <summary><c>offline-share-begin-button</c> — the initiator's dial + walk
    /// (<c>OfflineShareInitiateAsync</c>). An outlives-click op (test convention:
    /// it waits on the other person accepting), so this click handler returns
    /// immediately; the awaited status lands whenever the ceremony resolves. On
    /// a terminal status the host re-fetches the knock list always (the answered
    /// invitation leaves the pending list regardless of outcome) and refreshes
    /// the folder machine only when the ceremony actually landed a scope
    /// (<c>OfflineShareStatusLandsAScope</c> — the shared crate's own
    /// predicate, mirrors linux's <c>app.rs</c> <c>OfflineShareProgressed</c>
    /// call site).</summary>
    private async void OfflineShareBegin_Click(object sender, RoutedEventArgs e)
    {
        if (_rpc is null || App.CurrentOfflineShareSeat is not { } seat) return;
        var peerCode = OfflineSharePeerCodeInputBox.Text;
        CeremonyStatus status;
        try
        {
            status = await _rpc.OfflineShareInitiateAsync(seat, peerCode);
        }
        catch (Exception ex)
        {
            App.CurrentOfflineShareStatus = CeremonyStatus.Failed;
            ReportError(ex);
            Render();
            return;
        }
        // The outcome is painted BEFORE the host's refresh, and only the act
        // above can make it Failed: that refresh rides the nest, which an
        // offline ceremony may not have — waited on first, it left a delivered
        // ceremony reading "Not started" for as long as the nest was down.
        App.CurrentOfflineShareStatus = status;
        Render();
        if (_onActResolved is { } resolved) await resolved(status);
    }

    /// <summary><c>offline-receive-expect-button</c> — arm first-contact
    /// admission for the typed peer code (<c>FfiCeremonySeat.ExpectFrom</c>, a
    /// LOCAL, synchronous call — no nest round-trip). Without this, an offer
    /// from anyone is refused before any payload is parsed; this is the
    /// recipient's own explicit consent to be dialed by this one actor.</summary>
    private void OfflineReceiveExpect_Click(object sender, RoutedEventArgs e)
    {
        if (_crypto is null || App.CurrentOfflineShareSeat is not { } seat) return;
        var parsed = FaunaFfiMethods.OfflineShareParsePeerCode(OfflineSharePeerCodeInputBox.Text, _crypto.SecretBytes);
        if (parsed.actor.Length == 0) return;
        try
        {
            seat.ExpectFrom(parsed.actor);
            App.CurrentOfflineShareExpectingFrom = parsed.actor;
            App.CurrentOfflineShareStatus = CeremonyStatus.Expecting;
        }
        catch (Exception ex)
        {
            ReportError(ex);
        }
        Render();
    }

    /// <summary><c>offline-share-cancel-button</c> — close the panel. On the
    /// RECEIVE side it also withdraws the expectation
    /// (<c>CancelExpectation</c>, wormability rule 6) using the actor id
    /// recorded when Expect was pressed, not a re-parse of the (possibly since
    /// edited) peer-code box. Best-effort: an expectation that was never armed
    /// has nothing to withdraw. The seat itself is NOT dropped — it is reused
    /// if the user opens a panel again this session.</summary>
    private void OfflineShareCancel_Click(object sender, RoutedEventArgs e)
    {
        if (App.CurrentOfflineSharePanel == OfflineSharePanel.Receive
            && App.CurrentOfflineShareSeat is { } seat
            && App.CurrentOfflineShareExpectingFrom is { } expecting)
        {
            try { seat.CancelExpectation(expecting); } catch { /* best-effort */ }
            App.CurrentOfflineShareExpectingFrom = null;
        }
        App.CurrentOfflineSharePanel = OfflineSharePanel.Closed;
        App.CurrentOfflineShareStatus = CeremonyStatus.Idle;
        OfflineSharePeerCodeInputBox.Text = string.Empty;
        Render();
    }
}
