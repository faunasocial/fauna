using System;
using FaunaApp.Core.Services;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Helpers;

/// <summary>
/// The region placeholder painted <b>in place of</b> a region-withheld item on a
/// surface built imperatively (the post-detail dialog) — region-blocking.md § The
/// blocked render and the transparency surface: the app's frame naming the region
/// and its authority (<c>region-blocked-notice</c>), the authority's name
/// (<c>region-blocked-authority</c>), and its reason verbatim
/// (<c>region-blocked-reason</c>); a <c>collapse</c> adds the
/// <c>region-collapsed-reveal-button</c>. The feed card's and the conversation
/// bubble's XAML templates paint the same four elements in the same order —
/// linux's <c>region::placeholder_box</c>.
/// </summary>
internal static class RegionPlaceholderPanel
{
    public static StackPanel Build(RegionPlaceholderModel placeholder, Action onReveal)
    {
        var panel = new StackPanel { Spacing = 4, Padding = new Thickness(8, 12, 8, 12) };
        panel.Children.Add(Line(Ids.RegionBlockedNotice, placeholder.NoticeText, italic: true));
        panel.Children.Add(Line(Ids.RegionBlockedAuthority, placeholder.AuthorityName, italic: false));
        panel.Children.Add(Line(Ids.RegionBlockedReason, placeholder.Reason, italic: false));
        if (!placeholder.IsBlock)
        {
            var reveal = new Button
            {
                Content = S.Get("region/reveal_button"),
                HorizontalAlignment = HorizontalAlignment.Left,
                Padding = new Thickness(8, 2, 8, 2),
                FontSize = 12,
            };
            AutomationProperties.SetAutomationId(reveal, Ids.RegionCollapsedRevealButton);
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
