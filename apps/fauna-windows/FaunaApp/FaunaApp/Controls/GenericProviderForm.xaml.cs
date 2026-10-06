using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using uniffi.fauna_onboarding_machine;
using Windows.System;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// Reusable per-provider credentials form. Mirrors
/// <c>apps/fauna-web/src/lib/setup/GenericProviderForm.svelte</c> and
/// <c>apps/fauna-linux/src/views/onboarding/generic_provider_form.rs</c>'s
/// <c>build_creds_only</c>: renders a TextBox/PasswordBox per
/// <see cref="FieldMetaPlain"/>, wires changes through <see cref="SetField"/>,
/// and reads current values via <see cref="GetField"/>. A <c>hosted-auth</c>
/// field (the bundled provider's device flow, onboarding.md § 4) renders as a
/// Button instead, driven by <see cref="HostedAuthLabel"/>/
/// <see cref="HostedAuthEnabled"/>/<see cref="HostedAuthRun"/> — repainted per
/// tick via <see cref="RefreshHostedAuthButtons"/>, since this control owns no
/// tick of its own.
///
/// The DNS / VPS pages drive this with their own page-level Verify and
/// Continue buttons; this control holds only the input rows.
///
/// AutomationId for child fields is the canonical cross-app COMPOSED id —
/// <c>{dns,vps}-credentials-form-{field.Id}</c> (e.g.
/// <c>dns-credentials-form-api-token</c>), queried UNSCOPED — matching
/// ui.yaml, linux's <c>populate_creds_into</c>, web, apple and android, and
/// what the shared onboarding journeys actually query. The prefix is the
/// wrapper's own AutomationId, set by the caller (e.g.
/// <c>dns-credentials-form</c>) on the &lt;controls:GenericProviderForm/&gt;
/// element, so the two can never disagree. Windows previously carried the
/// bare <c>field.Id</c> here, resolved via <c>scope="dns-credentials-form"</c>
/// — a per-app divergence (priority #1) that kept windows out of
/// <c>test_bundled_provider.py</c>; converged 2026-09-03.
/// </summary>
internal sealed partial class GenericProviderForm : UserControl
{
    public static readonly DependencyProperty FieldsProperty =
        DependencyProperty.Register(
            nameof(Fields),
            typeof(IReadOnlyList<FieldMetaPlain>),
            typeof(GenericProviderForm),
            new PropertyMetadata(null, OnFieldsChanged));

    /// <summary>
    /// The list of fields to render. Typically sourced from
    /// <c>OnboardingMachine.VisibleDnsFields()</c> /
    /// <c>VisibleVpsFields()</c>, which already filter by capability and the
    /// active checkboxes (`buy_domain` / `same_provider_for_vps`).
    /// </summary>
    public IReadOnlyList<FieldMetaPlain>? Fields
    {
        get => (IReadOnlyList<FieldMetaPlain>?)GetValue(FieldsProperty);
        set => SetValue(FieldsProperty, value);
    }

    public static readonly DependencyProperty SetFieldProperty =
        DependencyProperty.Register(
            nameof(SetField),
            typeof(Action<string, string>),
            typeof(GenericProviderForm),
            new PropertyMetadata(null, OnFieldsChanged));

    /// <summary>
    /// Invoked on TextBox/PasswordBox change: <c>(fieldId, newValue)</c>.
    /// Wired by callers to <c>m.SetDnsCred</c> / <c>m.SetVpsCred</c>.
    /// </summary>
    public Action<string, string>? SetField
    {
        get => (Action<string, string>?)GetValue(SetFieldProperty);
        set => SetValue(SetFieldProperty, value);
    }

    public static readonly DependencyProperty GetFieldProperty =
        DependencyProperty.Register(
            nameof(GetField),
            typeof(Func<string, string>),
            typeof(GenericProviderForm),
            new PropertyMetadata(null, OnFieldsChanged));

    /// <summary>
    /// Invoked to read a field's current value: <c>(fieldId) =&gt; value</c>.
    /// Used to seed the input on rebuild so back-and-forth navigation
    /// preserves what the user typed.
    /// </summary>
    public Func<string, string>? GetField
    {
        get => (Func<string, string>?)GetValue(GetFieldProperty);
        set => SetValue(GetFieldProperty, value);
    }

    // ── hosted-auth (bundled provider's device flow, onboarding.md § 4) ──
    // A `hosted-auth` field renders as a Button instead of an input — tui's
    // `hosted_auth_button` shape one-to-one: same derived id, same
    // begin → open → wait sequence, which the VM owns as ONE call. Not
    // registered against OnFieldsChanged (unlike Fields/SetField/GetField
    // above) since these delegates are
    // themselves cached once by the caller (OnboardingViewModel's
    // constructor) and never change reference on their own — live state
    // (label/pressability) is repainted by RefreshHostedAuthButtons(),
    // called from the page's own per-tick refresh (mirrors linux's
    // HostedAuthHandle/refresh_hosted_auth_buttons — GenericProviderForm
    // owns no tick of its own).

    public static readonly DependencyProperty HostedAuthLabelProperty =
        DependencyProperty.Register(nameof(HostedAuthLabel), typeof(Func<string, string>),
            typeof(GenericProviderForm), new PropertyMetadata(null));
    public Func<string, string>? HostedAuthLabel
    {
        get => (Func<string, string>?)GetValue(HostedAuthLabelProperty);
        set => SetValue(HostedAuthLabelProperty, value);
    }

    public static readonly DependencyProperty HostedAuthEnabledProperty =
        DependencyProperty.Register(nameof(HostedAuthEnabled), typeof(Func<string, bool>),
            typeof(GenericProviderForm), new PropertyMetadata(null));
    public Func<string, bool>? HostedAuthEnabled
    {
        get => (Func<string, bool>?)GetValue(HostedAuthEnabledProperty);
        set => SetValue(HostedAuthEnabledProperty, value);
    }

    public static readonly DependencyProperty HostedAuthRunProperty =
        DependencyProperty.Register(nameof(HostedAuthRun), typeof(Func<string, Action<string>, Task>),
            typeof(GenericProviderForm), new PropertyMetadata(null));
    /// <summary>Runs the WHOLE device flow — begin, hand the verification URL
    /// to the callback this control supplies (its one job: open a browser),
    /// then poll until the token lands. One delegate, not the former
    /// begin/wait pair: see <c>OnboardingViewModel.RunHostedAuth</c> for the
    /// measured reason the view no longer sequences the two phases itself.
    /// </summary>
    public Func<string, Action<string>, Task>? HostedAuthRun
    {
        get => (Func<string, Action<string>, Task>?)GetValue(HostedAuthRunProperty);
        set => SetValue(HostedAuthRunProperty, value);
    }

    /// <summary>One button per <c>hosted-auth</c> field this form is
    /// currently showing, for <see cref="RefreshHostedAuthButtons"/>'s
    /// per-tick repaint. Rebuilt alongside the rest of the form.</summary>
    private readonly Dictionary<string, Button> _hostedAuthButtons = new();

    public GenericProviderForm()
    {
        InitializeComponent();
    }

    /// <summary>
    /// Repaint every currently-shown <c>hosted-auth</c> button's label and
    /// pressability from the machine's live state — call from the caller's
    /// own per-tick refresh (<c>DnsConfigView</c>/<c>VpsConfigView</c>'s
    /// <c>ViewModel.PropertyChanged</c> handler), the same place
    /// <c>RefreshDnsProviderEnablement</c> already lives. Best-effort: a
    /// null delegate (not yet bound) or an empty button set is a silent
    /// no-op, matching every other delegate call in this control.
    /// </summary>
    public void RefreshHostedAuthButtons()
    {
        var label = HostedAuthLabel;
        var enabled = HostedAuthEnabled;
        if (label is null && enabled is null) return;
        foreach (var (fieldId, btn) in _hostedAuthButtons)
        {
            if (label is not null) btn.Content = label(fieldId);
            if (enabled is not null) btn.IsEnabled = enabled(fieldId);
        }
    }

    /// <summary>
    /// Any of <c>Fields</c> / <c>SetField</c> / <c>GetField</c> changing
    /// triggers a full rebuild — keeps the wiring simple and matches the
    /// Linux pattern of "rebuild on selected_provider_id change".
    /// </summary>
    private static void OnFieldsChanged(DependencyObject d, DependencyPropertyChangedEventArgs e)
        => ((GenericProviderForm)d).Rebuild();

    /// <summary>
    /// The field list currently RENDERED, as an order-sensitive signature —
    /// `null` before the first build. See <see cref="Rebuild"/> for why.
    /// </summary>
    private string? _renderedSignature;

    /// <summary>Everything about a field list that changes what this control
    /// renders: the id, the widget kind, and the label. Values are NOT part of
    /// it — they are read from the machine at build time and repainted from it
    /// afterwards.</summary>
    private static string SignatureOf(IReadOnlyList<FieldMetaPlain>? fields)
        => fields is null
            ? "<null>"
            : string.Join("", fields.Select(f => $"{f.@id}{f.@fieldType}{f.@labelKey}"));

    private void Rebuild()
    {
        // ⚠ Rebuild ONLY when the field list actually changed. `Fields` is
        // bound `{x:Bind ViewModel.VisibleDnsFields, Mode=OneWay}`, and that VM
        // property calls straight through to `visible_dns_fields()` — a FRESH
        // array on every read. The onboarding machine's observer fires
        // PropertyChanged on every tick, `Bindings.Update()` re-reads the
        // property, the new array is never reference-equal to the old, and the
        // DP change callback therefore fired on EVERY TICK: the whole form was
        // torn down and rebuilt several times a second.
        //
        // That is not merely wasteful — it is a correctness bug. The
        // `hosted-auth` button's Click handler runs a MULTI-SECOND async flow
        // (begin → open the browser → poll until the token lands), and the
        // machine ticks throughout it, so the very Button running that handler
        // was destroyed and replaced mid-`await`. Measured on
        // `test_bundled_provider.py --app windows` (2026-09-03): two runs of the
        // identical journey failed at two DIFFERENT points — one never issued
        // the token poll at all (stuck on `Pending`), the next reached
        // `Connected` but then lost the verify press. Non-determinism across
        // runs is the signature.
        //
        // The neighbouring `SetDnsCred`/`GetDnsCred` delegates are already
        // cached as VM FIELDS for exactly this reason ("without caching, the
        // form would lose focus every refresh") — this is the same lesson
        // applied to the list itself. Linux rebuilds its creds section only on
        // an actual provider change (`dns_config.rs`), which is the shape this
        // restores (priority #1).
        var signature = SignatureOf(Fields);
        if (signature == _renderedSignature) return;
        _renderedSignature = signature;

        FieldsPanel.Children.Clear();
        _hostedAuthButtons.Clear();
        var fields = Fields;
        if (fields is null) return;

        var getter = GetField;
        var setter = SetField;

        // Canonical cross-app field test id: `{kind}-credentials-form-{field.id}`
        // (linux's `populate_creds_into` composes the same string from its
        // `kind` argument). Here the prefix is read back off the wrapper's own
        // AutomationId — the caller already spells it `dns-credentials-form` /
        // `vps-credentials-form` in XAML — so there is no second copy of the
        // kind to drift out of step with it.
        var formId = (string?)GetValue(AutomationProperties.AutomationIdProperty) ?? "";

        foreach (var f in fields)
        {
            var label = ResolveLabel(f.@labelKey);
            var testId = formId.Length > 0 ? $"{formId}-{f.@id}" : f.@id;

            if (f.@fieldType == FieldTypePlain.HostedAuth)
            {
                // The bundled provider's hosted sign-in (onboarding.md § 4) —
                // a button, not an input, mirroring tui's hosted_auth_button
                // / linux's populate_creds_into one-to-one: same derived id,
                // same begin -> open -> wait sequence. Button has no built-in
                // Header, unlike TextBox/PasswordBox, so a plain TextBlock
                // carries the field label above it (linux's row shape).
                var fieldId = f.@id;
                var btn = new Button
                {
                    Content = HostedAuthLabel?.Invoke(fieldId) ?? "",
                    IsEnabled = HostedAuthEnabled?.Invoke(fieldId) ?? false,
                };
                btn.SetValue(AutomationProperties.AutomationIdProperty, testId);
                btn.Click += async (_, _) =>
                {
                    // ONE call for the whole flow. The VM begins the device
                    // authorization, hands the verification URL back through
                    // the callback below, and polls until the token lands —
                    // this control only knows how to open a browser. Errors are
                    // already the field's HostedAuthState.Failed, painted on the
                    // next RefreshHostedAuthButtons tick, so there is nothing to
                    // surface here.
                    var run = HostedAuthRun;
                    if (run is null) return;
                    // The shell's one opener seam (`Services.UrlOpener`): parse,
                    // fire-and-forget, and — under a harness launch — record
                    // instead of reaching the OS. Both halves matter here. The
                    // fire-and-forget half is why a missing/hung default-handler
                    // association cannot stall the token poll that follows (the
                    // verification_url is painted in the button's own Pending
                    // label, so the user can always reach it by hand); the
                    // suppression half is why the real browser can no longer
                    // wedge the fake provider server this journey polls.
                    await run(fieldId, url => FaunaApp.Services.UrlOpener.Open(url, "Onboarding"));
                };

                var row = new StackPanel { Spacing = 4 };
                row.Children.Add(new TextBlock { Text = label });
                row.Children.Add(btn);
                FieldsPanel.Children.Add(row);
                _hostedAuthButtons[fieldId] = btn;
                continue;
            }

            var initial = getter?.Invoke(f.@id) ?? "";

            FrameworkElement input;
            if (f.@fieldType == FieldTypePlain.Secret)
            {
                var pb = new PasswordBox
                {
                    Header = label,
                    Password = initial,
                };
                pb.PasswordChanged += (_, _) => setter?.Invoke(f.@id, pb.Password);
                input = pb;
            }
            else
            {
                // Text and Select both render as plain TextBox for now.
                // Select-typed fields don't appear in `visible_*_fields()`
                // results today (post-verify dropdowns are emitted as
                // separate widgets after VerifyDns succeeds), so this branch
                // matches Linux's `build_creds_only` behavior.
                var tb = new TextBox
                {
                    Header = label,
                    Text = initial,
                };
                tb.TextChanged += (_, _) => setter?.Invoke(f.@id, tb.Text);
                input = tb;
            }

            // Field IDs are stable strings from providers_generated.rs; the
            // `{kind}-credentials-form-` prefix disambiguates the duplicate
            // field ids the DNS and VPS forms share (`api-token` on both),
            // which is why the shared journeys can query them unscoped.
            input.SetValue(AutomationProperties.AutomationIdProperty, testId);

            FieldsPanel.Children.Add(input);
        }
    }

    /// <summary>
    /// Resolve a provisioning label key — the Rust state machine emits
    /// dotted keys (<c>provisioning.cloudflare.api_token</c>) but our resw
    /// uses slashes (<c>provisioning/cloudflare/api_token</c>). Convert and
    /// fall back to the raw key on miss so the form is never blank-headed.
    /// </summary>
    private static string ResolveLabel(string labelKey)
    {
        var slashed = labelKey.Replace('.', '/');
        var resolved = S.Get(slashed);
        return resolved == slashed ? labelKey : resolved;
    }
}
