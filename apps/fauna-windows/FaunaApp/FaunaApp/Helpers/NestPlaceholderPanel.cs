using System;
using FaunaApp.Core.Services;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Helpers;

/// <summary>
/// The nest placeholder painted <b>in place of</b> an item a rule of the nest it was
/// read through withholds, on a surface built imperatively (the post-detail dialog)
/// — nest-rules.md § The nest source in the engine; § Element IDs: the app's frame
/// naming the nest (<c>nest-blocked-notice</c>, its <c>verdict</c> attribute on
/// HelpText), the nest's domain (<c>nest-blocked-domain</c>), and the admin's reason
/// verbatim (<c>nest-blocked-reason</c>, never an i18n string); a <c>collapse</c>
/// adds the <c>nest-collapsed-reveal-button</c>, a <c>block</c> has none (a member
/// never loosens a nest rule). <see cref="RegionPlaceholderPanel"/>'s twin; the
/// feed card's and the conversation bubble's XAML paint the same four elements in
/// the same order — android's <c>NestPlaceholder</c>.
/// </summary>
internal static class NestPlaceholderPanel
{
    public static StackPanel Build(NestPlaceholderModel placeholder, Action onReveal)
    {
        var panel = new StackPanel { Spacing = 4, Padding = new Thickness(8, 12, 8, 12) };
        var notice = Line(Ids.NestBlockedNotice, placeholder.NoticeText, italic: true);
        AutomationProperties.SetHelpText(notice, placeholder.Verb);
        panel.Children.Add(notice);
        panel.Children.Add(Line(Ids.NestBlockedDomain, placeholder.Domain, italic: false));
        panel.Children.Add(Line(Ids.NestBlockedReason, placeholder.Reason, italic: false));
        if (!placeholder.IsBlock)
        {
            var reveal = new Button
            {
                Content = S.Get("nest_rules/reveal_button"),
                HorizontalAlignment = HorizontalAlignment.Left,
                Padding = new Thickness(8, 2, 8, 2),
                FontSize = 12,
            };
            AutomationProperties.SetAutomationId(reveal, Ids.NestCollapsedRevealButton);
            reveal.Click += (_, _) => onReveal();
            panel.Children.Add(reveal);
        }
        return panel;
    }

    private static TextBlock Line(string id, string text, bool italic)
    {
        var line = new TextBlock
        {
            Text = text,
            TextWrapping = TextWrapping.Wrap,
            Opacity = 0.6,
            FontSize = 12,
        };
        if (italic) line.FontStyle = Windows.UI.Text.FontStyle.Italic;
        AutomationProperties.SetAutomationId(line, id);
        AutomationProperties.SetName(line, text);
        return line;
    }
}
