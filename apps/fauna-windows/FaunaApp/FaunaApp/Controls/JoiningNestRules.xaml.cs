using System.Collections.Generic;
using System.Linq;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using LocalizedText = uniffi.fauna_core.LocalizedText;
using NestRulesSummary = uniffi.fauna_client_policy.NestRulesSummary;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Controls;

/// <summary>
/// <c>onboarding-nest-rules</c> — the joining surface (<c>nest-rules.md</c>
/// § Acceptance at joining, <i>Joining is acceptance</i>): <c>invite_request</c>,
/// and <c>handle_entry</c> while its Continue signs in, show the nest's rules
/// directly above the by-continuing <see cref="TermsNotice"/>.
///
/// <para><b>A paint, deciding nothing.</b> The shared machine's
/// <c>OnboardingMachine::joining_nest_rules</c> already decided whether this page
/// shows rules (the step, <c>terms_notice_visible</c>, the verified pre-sign-in
/// read, an applied empty document painting nothing) and chose every label; this
/// paints what it answers: the heading naming the nest — the not-applied sentence
/// after it when none of the document applies here — then one indexed
/// <c>onboarding-nest-rules-item</c> per rule. tui's
/// <c>nest_rules::joining_elements</c>, linux's <c>nest_rules::paint_joining</c>,
/// web's <c>NestRulesJoining.svelte</c>, android's <c>NestRulesJoining</c>, apple's
/// <c>JoiningNestRules</c>.</para>
///
/// <para>Every <see cref="TextBlock"/> carries an AutomationId and no
/// AutomationProperties.Name, so the e2e <c>get_text</c> reads the painted text.
/// Absent is Collapsed, which leaves no UIA peer (<c>count == 0</c>).</para>
/// </summary>
public sealed partial class JoiningNestRules : UserControl
{
    /// The lines last painted (heading first, then one per rule), or <c>null</c>
    /// while collapsed — the host repaints on every machine tick, and an unchanged
    /// answer must not rebuild the automation tree under a test's read.
    private string[]? _painted;

    public JoiningNestRules()
    {
        InitializeComponent();
        Visibility = Visibility.Collapsed;
    }

    /// <summary>Paint <paramref name="summary"/> — the host page's
    /// <c>OnboardingViewModel.JoiningNestRules</c>; <c>null</c> collapses the
    /// surface. Cheap to call on every snapshot change: an answer whose painted
    /// text is unchanged touches nothing.</summary>
    internal void Paint(NestRulesSummary? summary)
    {
        var lines = summary is null ? null : Lines(summary);
        if (lines is null ? _painted is null : _painted is not null && lines.SequenceEqual(_painted))
            return;
        _painted = lines;

        Rows.Children.Clear();
        if (lines is null)
        {
            Visibility = Visibility.Collapsed;
            return;
        }
        Rows.Children.Add(Line(Ids.OnboardingNestRules, lines[0], item: false));
        foreach (var line in lines.Skip(1))
            Rows.Children.Add(Line(Ids.OnboardingNestRulesItem, line, item: true));
        Visibility = Visibility.Visible;
    }

    /// The heading (with the not-applied sentence after it, when set), then one
    /// line per rule — a rule this device cannot apply carrying its not-applied
    /// note under its line.
    private static string[] Lines(NestRulesSummary summary)
    {
        var heading = S.Resolve(Text("nest_rules.joining_heading", ("nest", summary.domain)));
        var text = summary.notAppliedSentence is { } why
            ? $"{heading} {S.Resolve(why)}"
            : heading;
        return new[] { text }.Concat(summary.rules.Select(r => FaunaApp.Core.Services.NestRulesHost.JoiningRuleLine(r))).ToArray();
    }

    private static LocalizedText Text(string key, params (string Name, string Value)[] args)
        => new(key, args.ToDictionary(a => a.Name, a => a.Value));

    private static TextBlock Line(string id, string text, bool item)
    {
        var block = new TextBlock
        {
            Text = text,
            TextWrapping = TextWrapping.Wrap,
        };
        if (item)
        {
            block.FontSize = 12;
            block.Opacity = 0.7;
        }
        AutomationProperties.SetAutomationId(block, id);
        return block;
    }
}
