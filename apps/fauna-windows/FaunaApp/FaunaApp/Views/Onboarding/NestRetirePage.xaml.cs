using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Fauna.Generated;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Helpers;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Navigation;
using uniffi.fauna_onboarding_machine;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Views.Onboarding;

/// <summary>
/// What opens the <c>nest_retire</c> page: which entry, the saved nest's URL (an
/// attribution candidate and the <i>current</i> badge's host), the admin entry's
/// live read of the nest's active local domains, and where leaving goes —
/// <c>OnExit(sessionDropped)</c> is told whether this session's own nest was
/// retired from <c>admin-nest</c>, in which case the host ends the session keeping
/// credentials and re-runs launch routing (<c>nest-retirement.md</c> § Where logic
/// lives); otherwise it shows the entry again.
/// </summary>
internal sealed record NestRetireContext(
    RetireEntry Entry,
    string? NestUrl,
    Func<Task<IReadOnlyList<string>>> ActiveLocalDomains,
    Action<bool> OnExit);

/// <summary>
/// nest_retire — retire an app-provisioned nest
/// (<c>docs/goal/behavior/nest-retirement.md</c>; ui.yaml <c>onboarding.nest_retire</c>).
/// A paint shell over the shared <c>NestRetireMachine</c> through
/// <see cref="NestRetireViewModel"/>: every state, sentence and gate is the
/// machine's; this page paints the snapshot and forwards gestures (priority #2).
/// The windows lift of tui's <c>wizard/nest_retire.rs</c>, FaunaKit's
/// <c>NestRetireView</c> and android's <c>NestRetireScreen</c>.
///
/// <para>Hosted in the frame that holds its entry, as the wizard family's pages
/// are on windows: the root frame over <c>launch_retry</c>, the admin shell's
/// content frame over <c>admin-nest</c> (the live session stays up behind it).
/// The machine's async calls move the snapshot while they run, so the page
/// repaints on a short timer while it is up, besides every gesture.</para>
///
/// <para>The generated record members are read without their <c>@</c> escape
/// (none is a C# keyword): the <c>x.@y.z</c> spelling trips the email guard.</para>
/// </summary>
public sealed partial class NestRetirePage : Page
{
    /// <summary>Repaint cadence while the page is up (linux's 150 ms).</summary>
    private static readonly TimeSpan RepaintInterval = TimeSpan.FromMilliseconds(150);

    private readonly NestRetireViewModel _vm = new();
    private NestRetireContext? _context;
    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _repaint;
    private bool _exited;

    /// <summary>Provider id → its row button, built once.</summary>
    private readonly List<(string Id, Button Button)> _providerButtons = new();
    private string? _formBoundForProvider;
    private string? _rowsSignature;
    private string _leftover = "";

    /// <summary>The two step rows' painted parts, built once (DNS = 0, Server = 1).</summary>
    private readonly List<(TextBlock Glyph, TextBlock Label, TextBlock Note, TextBlock Error)> _stepRows = new();

    public NestRetirePage()
    {
        InitializeComponent();
        CredsForm.SetField = (id, value) => _vm.SetCred(id, value);
        CredsForm.GetField = id => _vm.GetCred(id);
        CredsForm.HostedAuthLabel = HostedAuthLabel;
        CredsForm.HostedAuthEnabled = id => _vm.Machine?.HostedAuthCanBegin(id) ?? false;
        CredsForm.HostedAuthRun = (id, openUrl) => _vm.HostedAuthRun(id, openUrl);
        BuildProviderRow();
        BuildStepRows();
        _vm.Changed += () => DispatcherQueue.TryEnqueue(Render);
    }

    // ── e2e provider overrides ────────────────────────────────────────────
#if DEBUG || FAUNA_E2E_AGENT
    private static readonly object s_e2eLock = new();
    private static (string? Vps, string? Doh) s_e2eBaseUrls;

    /// <summary>
    /// Record the <c>vps</c> and <c>doh</c> entries of a <c>set_provider_base_urls</c>
    /// bridge call. The machine takes them at construction, and the call arrives
    /// on the launch retry surface and post-auth, where no onboarding machine is
    /// up to hold them — so the test agent records them here too (linux's
    /// <c>remember_e2e_provider_base_urls</c>). Compiled out of a release build
    /// with the rest of the automation surface (convention 15).
    /// </summary>
    internal static void RememberE2eProviderBaseUrls(string jsonArg)
    {
        string? vps = null, doh = null;
        try
        {
            using var doc = System.Text.Json.JsonDocument.Parse(jsonArg);
            if (doc.RootElement.ValueKind == System.Text.Json.JsonValueKind.Object)
            {
                if (doc.RootElement.TryGetProperty("vps", out var v)) vps = v.GetString();
                if (doc.RootElement.TryGetProperty("doh", out var d)) doh = d.GetString();
            }
        }
        catch (System.Text.Json.JsonException) { /* an unparsable map overrides nothing */ }
        lock (s_e2eLock) s_e2eBaseUrls = (vps, doh);
    }
#endif

    /// <summary><c>(vps, doh)</c> — both <c>null</c> in a release build.</summary>
    private static (string? Vps, string? Doh) ProviderBaseUrlOverrides()
    {
#if DEBUG || FAUNA_E2E_AGENT
        lock (s_e2eLock) return s_e2eBaseUrls;
#else
        return (null, null);
#endif
    }

    // ── lifecycle ─────────────────────────────────────────────────────────

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        if (e.Parameter is not NestRetireContext ctx) return;
        _context = ctx;
        _exited = false;
        var (vps, doh) = ProviderBaseUrlOverrides();
        ShellLog.Info("NestRetire", $"[retire] page opened from {ctx.Entry}");
        _ = _vm.Open(ctx.Entry, ctx.NestUrl, vps, doh, ctx.ActiveLocalDomains);
        _repaint = DispatcherQueue.CreateTimer();
        _repaint.Interval = RepaintInterval;
        _repaint.IsRepeating = true;
        _repaint.Tick += (_, _) => Render();
        _repaint.Start();
        Render();
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        base.OnNavigatedFrom(e);
        _repaint?.Stop();
        _repaint = null;
        // Navigated away by anything but our own exit (a launch re-route, say):
        // the token and every fetched code still never outlive the visit.
        if (!_exited)
        {
            _exited = true;
            _vm.Leave();
        }
        App.CurrentErrorMessage = null;
    }

    private void Exit()
    {
        if (_exited) return;
        _exited = true;
        _repaint?.Stop();
        var dropSession = _vm.Leave();
        ShellLog.Info("NestRetire", $"[retire] page left (session dropped: {dropSession})");
        App.CurrentErrorMessage = null;
        _context?.OnExit(dropSession);
    }

    // ── one-time builds ───────────────────────────────────────────────────

    private void BuildProviderRow()
    {
        foreach (var p in Providers.All.Where(p => p.Capabilities.Contains(Capability.Vps)))
        {
            var btn = new Button { Content = ResolveKey(p.DisplayNameKey) };
            btn.SetValue(AutomationProperties.AutomationIdProperty, $"vps-provider-row[{p.Id}]");
            var pid = p.Id;
            btn.Click += (_, _) => _vm.SelectProvider(pid);
            VpsProviderRow.Children.Add(btn);
            _providerButtons.Add((pid, btn));
        }
    }

    private void BuildStepRows()
    {
        for (var i = 0; i < 2; i++)
        {
            var glyph = new TextBlock { Width = 20 };
            glyph.SetValue(AutomationProperties.AutomationIdProperty, "provisioning-step-checkbox");
            var label = new TextBlock();
            label.SetValue(AutomationProperties.AutomationIdProperty, "provisioning-step-label");
            var note = new TextBlock { Opacity = 0.7, TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
            note.SetValue(AutomationProperties.AutomationIdProperty, "provisioning-substep");
            var error = new TextBlock { TextWrapping = TextWrapping.Wrap, Visibility = Visibility.Collapsed };
            error.SetValue(AutomationProperties.AutomationIdProperty, "provisioning-step-error");
            if (Application.Current.Resources.TryGetValue("SystemErrorTextColor", out var red)
                && red is Windows.UI.Color color)
                error.Foreground = new Microsoft.UI.Xaml.Media.SolidColorBrush(color);

            var text = new StackPanel { Spacing = 2 };
            text.Children.Add(label);
            text.Children.Add(note);
            text.Children.Add(error);
            var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8, Margin = new Thickness(0, 2, 0, 2) };
            row.SetValue(AutomationProperties.AutomationIdProperty, "provisioning-step-row");
            row.SetValue(AutomationProperties.NameProperty, " ");
            row.Children.Add(glyph);
            row.Children.Add(text);
            StepsPanel.Children.Add(row);
            _stepRows.Add((glyph, label, note, error));
        }
    }

    // ── paint ─────────────────────────────────────────────────────────────

    private void Render()
    {
        if (_exited || _vm.Machine is not { } m) return;
        var snap = m.Snapshot();
        var phase = snap.phase;
        var steps = snap.steps;

        var credentials = phase is RetirePhase.Credentials or RetirePhase.Listing;
        Show(CredentialsPanel, credentials);
        Show(ListPanel, phase == RetirePhase.List);
        Show(ConfirmPanel, phase == RetirePhase.Confirm);
        var running = phase == RetirePhase.Running;
        var done = phase == RetirePhase.Done;
        Show(StepsPanel, running || done);
        Show(RunningPanel, running);
        Show(DonePanel, done);
        Show(DoneHeadline, done && snap.retired is not null);

        var failed = steps.Any(s => s.state is StepState.Failed);
        // Back: credentials and list; a run only once it stopped on a failure —
        // never while a step is in flight; never on confirm (Cancel) or done (Done).
        Show(BackButton, credentials || phase == RetirePhase.List || (running && failed));

        if (credentials) RenderCredentials(m, phase == RetirePhase.Listing);
        if (phase == RetirePhase.List) RenderList(m, snap);
        if (phase == RetirePhase.Confirm) RenderConfirm(m, snap);
        if (running || done) RenderSteps(m, steps);
        if (running)
        {
            Show(RetryButton, failed);
            Show(ForceServerWarning, snap.forceServerOffered);
            Show(ForceServerButton, snap.forceServerOffered);
        }
        if (done) RenderDone(m, snap);

        // The page's error: the machine's own, painted on error-message and
        // mirrored onto the app-wide state-protocol surface (as OnboardingPage).
        var error = string.IsNullOrEmpty(snap.error) ? null : snap.error;
        if (ErrorBar.Message != error) ErrorBar.Message = error;
        if (ErrorBar.IsOpen != (error is not null)) ErrorBar.IsOpen = error is not null;
        App.CurrentErrorMessage = error;
    }

    private static void Show(UIElement element, bool visible)
    {
        var v = visible ? Visibility.Visible : Visibility.Collapsed;
        if (element.Visibility != v) element.Visibility = v;
    }

    private void RenderCredentials(INestRetireMachine m, bool listing)
    {
        var selected = m.SelectedProvider();
        foreach (var (id, btn) in _providerButtons)
        {
            btn.IsEnabled = !listing;
            var on = id == selected;
            btn.SetValue(AutomationProperties.HelpTextProperty, on ? "on" : "off");
            var style = on && Application.Current.Resources.TryGetValue("AccentButtonStyle", out var accent)
                ? accent as Style
                : null;
            if (btn.Style != style) btn.Style = style;
        }
        Show(ProviderSection, selected is not null);
        if (selected is null)
        {
            _formBoundForProvider = null;
            return;
        }
        if (_formBoundForProvider != selected)
        {
            var p = Providers.All.FirstOrDefault(x => x.Id == selected);
            if (p is not null)
            {
                ProviderLink.Content = p.SignupUrl;
                ProviderHelpText.Text = ResolveKey(p.HelpKey);
            }
            CredsForm.Fields = m.VisibleFields();
            _formBoundForProvider = selected;
        }
        CredsForm.IsEnabled = !listing;
        CredsForm.RefreshHostedAuthButtons();
        VerifyButton.IsEnabled = m.CanVerify() && !listing;
        Show(ListingRing, listing);
        ListingRing.IsActive = listing;
    }

    private void RenderList(INestRetireMachine m, RetireSnapshot snap)
    {
        var servers = snap.servers;
        var selectedId = snap.selected;
        Show(EmptyMessage, servers.Length == 0);
        Show(ListLabel, servers.Length > 0);
        DeleteButton.IsEnabled = selectedId is not null;

        // Rebuild the rows only when what they show changed — a rebuild under a
        // pending click would drop it.
        var signature = string.Join("\n", servers.Select(r => RowSignature(r, selectedId)));
        if (signature == _rowsSignature) return;
        _rowsSignature = signature;
        ServerList.Children.Clear();
        foreach (var row in servers)
            ServerList.Children.Add(BuildServerRow(m, row, row.serverId == selectedId));
    }

    private static string RowSignature(ManagedServerRow r, string? selectedId)
    {
        var others = string.Join(",", r.secondaryDomains);
        return $"{r.serverId}|{r.name}|{r.domain}|{others}|{r.ipv4}|{r.current}|{r.marked}"
            + $"|{r.transferCode}|{r.serverId == selectedId}";
    }

    private Button BuildServerRow(INestRetireMachine m, ManagedServerRow row, bool selected)
    {
        var content = new StackPanel { Spacing = 4 };
        var header = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
        header.Children.Add(new TextBlock { Text = selected ? "◉" : "○" });
        header.Children.Add(Text("retire-server-name", row.name, bold: true));
        content.Children.Add(header);

        var facts = new StackPanel { Spacing = 2, Margin = new Thickness(28, 0, 0, 0) };
        if (row.domain is { } domain)
            facts.Children.Add(Fact("retire-server-domain", S.Get("onboarding/retire/domain_label"), domain));
        var others = row.secondaryDomains;
        if (others.Length > 0)
            facts.Children.Add(Fact("retire-server-secondary-domains",
                S.Get("onboarding/retire/secondary_domains_label"), string.Join(", ", others)));
        if (row.ipv4 is { } ipv4)
            facts.Children.Add(Fact("retire-server-address", S.Get("onboarding/retire/address_label"), ipv4));
        if (row.current)
            facts.Children.Add(Text("retire-server-current-badge", S.Get("onboarding/retire/current_badge"), bold: true));
        if (!row.marked)
            facts.Children.Add(Text("retire-server-unmarked-note", S.Get("onboarding/retire/unmarked_note")));
        content.Children.Add(facts);
        if (selected) content.Children.Add(TransferCode(m, row));

        var btn = new Button
        {
            Content = content,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            HorizontalContentAlignment = HorizontalAlignment.Stretch,
        };
        btn.SetValue(AutomationProperties.AutomationIdProperty, "retire-server-item");
        btn.SetValue(AutomationProperties.NameProperty, row.name);
        btn.SetValue(AutomationProperties.HelpTextProperty, selected ? "on" : "off");
        var id = row.serverId;
        btn.Click += (_, _) => _vm.Select(id);
        return btn;
    }

    /// <summary>The transfer-code affordance on the selected row — independent of deleting.</summary>
    private StackPanel TransferCode(INestRetireMachine m, ManagedServerRow row)
    {
        var panel = new StackPanel { Spacing = 4, Margin = new Thickness(28, 0, 0, 0) };
        switch (row.transferCode)
        {
            case TransferCodeState.Idle:
            {
                var fetch = new Button { Content = S.Get("onboarding/retire/transfer_code_button") };
                fetch.SetValue(AutomationProperties.AutomationIdProperty, "retire-transfer-code-button");
                fetch.Click += (_, _) => _vm.FetchTransferCode();
                panel.Children.Add(fetch);
                break;
            }
            case TransferCodeState.Code held:
            {
                var code = held.code;
                var line = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 8 };
                line.Children.Add(new TextBlock
                {
                    Text = S.Get("onboarding/retire/transfer_code_label") + ":",
                    VerticalAlignment = VerticalAlignment.Center,
                });
                var value = Text("retire-transfer-code-value", code);
                value.VerticalAlignment = VerticalAlignment.Center;
                value.IsTextSelectionEnabled = true;
                line.Children.Add(value);
                var copy = new Button { Content = S.Get("onboarding/retire/transfer_code_copy") };
                copy.SetValue(AutomationProperties.AutomationIdProperty, "retire-transfer-code-copy-button");
                copy.Click += (_, _) => ClipboardHelper.CopyText(code);
                line.Children.Add(copy);
                panel.Children.Add(line);
                break;
            }
        }
        if (m.TransferCodeNote(row.serverId) is { } note)
            panel.Children.Add(Text("retire-transfer-code-status", S.Resolve(note)));
        return panel;
    }

    private static TextBlock Text(string automationId, string text, bool bold = false)
    {
        var tb = new TextBlock { Text = text, TextWrapping = TextWrapping.Wrap };
        if (bold) tb.FontWeight = Microsoft.UI.Text.FontWeights.SemiBold;
        tb.SetValue(AutomationProperties.AutomationIdProperty, automationId);
        return tb;
    }

    private static StackPanel Fact(string automationId, string label, string value)
    {
        var line = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        line.Children.Add(new TextBlock { Text = label + ":", Opacity = 0.7 });
        line.Children.Add(Text(automationId, value));
        return line;
    }

    private void RenderConfirm(INestRetireMachine m, RetireSnapshot snap)
    {
        // The summary's provider rides as its display KEY, so it is resolved nested.
        var summary = string.Join("\n", m.ConfirmSummary().Select(S.ResolveNested));
        if (ConfirmSummary.Text != summary) ConfirmSummary.Text = summary;
        // The name box is never written from the repaint: it is cleared by the
        // gesture that enters this state (OnDeleteClick), and while the state is
        // up the box is the source. WinUI raises TextChanged asynchronously, so a
        // repaint that pushed the view-model's copy back between a fill and its
        // TextChanged wiped what was typed (measured 2026-11-04: the typed name
        // lost, the confirm button dead at the click).
        ConfirmButton.IsEnabled = snap.confirmEnabled;
    }

    private void RenderSteps(INestRetireMachine m, RetireStepRow[] steps)
    {
        for (var i = 0; i < _stepRows.Count && i < steps.Length; i++)
        {
            var step = steps[i];
            var state = step.state;
            var (glyph, label, note, error) = _stepRows[i];
            var g = ValueFormat.ProvisioningStatusGlyph(FaunaOnboardingMachineMethods.RetireStepStatus(state));
            if (glyph.Text != g) glyph.Text = g;
            var l = S.Resolve(m.StepLabel(step.step));
            if (label.Text != l) label.Text = l;
            var n = m.StepNote(state) is { } stepNote ? S.Resolve(stepNote) : null;
            if (note.Text != (n ?? "")) note.Text = n ?? "";
            Show(note, n is not null);
            var cause = state is StepState.Failed f ? f.cause : null;
            if (error.Text != (cause ?? "")) error.Text = cause ?? "";
            Show(error, cause is not null);
        }
    }

    private void RenderDone(INestRetireMachine m, RetireSnapshot snap)
    {
        if (snap.retired is { } retired)
        {
            var headline = S.Format("onboarding/retire/done_deleted", retired.name);
            if (DoneHeadline.Text != headline) DoneHeadline.Text = headline;
        }
        _leftover = string.Join("\n", m.LeftoverLines().Select(S.Resolve));
        if (LeftoverText.Text != _leftover) LeftoverText.Text = _leftover;
    }

    // ── gestures ──────────────────────────────────────────────────────────

    private string HostedAuthLabel(string fieldId)
        => _vm.Machine?.HostedAuthState(fieldId) switch
        {
            HostedAuthState.Pending p => S.Format("provisioning/hosted_auth/pending", p.userCode),
            HostedAuthState.Connected => S.Get("provisioning/hosted_auth/connected"),
            HostedAuthState.Failed f => S.Format("provisioning/hosted_auth/failed", f.message),
            _ => S.Get("provisioning/hosted_auth/connect"),
        };

    private void OnProviderLinkClick(object sender, RoutedEventArgs e)
    {
        var pid = _vm.Machine?.SelectedProvider();
        var p = Providers.All.FirstOrDefault(x => x.Id == pid);
        if (p is not null) FaunaApp.Services.UrlOpener.Open(p.SignupUrl, "NestRetire");
    }

    private void OnVerifyClick(object sender, RoutedEventArgs e) => _vm.Verify();
    /// <summary>Enter the confirm state untyped: the box is cleared here, by the
    /// gesture, never by the repaint (<see cref="RenderConfirm"/>).</summary>
    private void OnDeleteClick(object sender, RoutedEventArgs e)
    {
        _vm.BeginConfirm();
        ConfirmNameInput.Text = "";
    }

    private void OnConfirmNameChanged(object sender, TextChangedEventArgs e)
    {
        // The clear above re-raises this (asynchronously) with the value the
        // view-model already holds: nothing to forward.
        if (ConfirmNameInput.Text == _vm.ConfirmTyped) return;
        _vm.SetConfirmName(ConfirmNameInput.Text);
    }

    private void OnConfirmClick(object sender, RoutedEventArgs e) => _vm.Confirm();
    private void OnCancelClick(object sender, RoutedEventArgs e) => _vm.CancelConfirm();
    private void OnRetryClick(object sender, RoutedEventArgs e) => _vm.Retry();
    private void OnForceServerClick(object sender, RoutedEventArgs e) => _vm.ForceServer();
    private void OnLeftoverCopyClick(object sender, RoutedEventArgs e) => ClipboardHelper.CopyText(_leftover);
    private void OnDoneClick(object sender, RoutedEventArgs e) => Exit();

    private void OnBackClick(object sender, RoutedEventArgs e)
    {
        if (_vm.Back()) Exit();
    }

    /// <summary>A dotted registry key resolved through the resw (slashed), falling
    /// back to the raw key on a miss — the vps_config shape.</summary>
    private static string ResolveKey(string dotted)
    {
        var slashed = dotted.Replace('.', '/');
        var resolved = S.Get(slashed);
        return resolved == slashed ? dotted : resolved;
    }
}
