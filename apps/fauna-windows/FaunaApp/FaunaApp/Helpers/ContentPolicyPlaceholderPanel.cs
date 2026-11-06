using System;
using FaunaApp.UiIds;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using S = FaunaApp.Core.Services.Strings;

namespace FaunaApp.Helpers;

/// <summary>
/// The family-policy placeholder painted <b>in place of</b> a guardian- or
/// threshold-withheld item on a surface built imperatively (the post-detail dialog)
/// — family-safety.md § Content policy; nest-rules.md § The shared rule editor →
/// <i>The reason is the member's to read</i>: the fixed frame
/// (<c>content-policy-blocked-notice</c>, its <c>state</c> attribute — <c>blocked</c>
/// | <c>collapsed</c> — on HelpText), the guardian rule's reason verbatim beside it
/// (<c>content-policy-blocked-reason</c>, absent when a floor, the viewer's own
/// threshold or a report drove the verdict), and on a <c>collapse</c> the
/// <c>content-policy-collapsed-reveal-button</c>. The feed card's and the
/// conversation bubble's XAML paint the same elements in the same order — android's
/// post-detail family arm.
/// </summary>
internal static class ContentPolicyPlaceholderPanel
{
    /// <param name="collapsed">A <c>collapse</c> (revealable) rather than a
    /// <c>block</c>.</param>
    /// <param name="noticeText">The frame: the family-policy sentence, or "You
    /// reported this" when the viewer's own report hid the item.</param>
    /// <param name="reason">The guardian rule's reason, or <c>null</c>.</param>
    /// <param name="onReveal">The collapse reveal (the shared content-policy reveal
    /// set); unused for a block.</param>
    public static StackPanel Build(bool collapsed, string noticeText, string? reason, Action onReveal)
    {
        var panel = new StackPanel { Spacing = 4, Padding = new Thickness(8, 12, 8, 12) };
        var notice = Line(Ids.ContentPolicyBlockedNotice, noticeText, italic: true);
        AutomationProperties.SetHelpText(notice, collapsed ? "collapsed" : "blocked");
        panel.Children.Add(notice);
        if (reason is { Length: > 0 })
            panel.Children.Add(Line(Ids.ContentPolicyBlockedReason, reason, italic: false));
        if (collapsed)
        {
            var label = S.Get("family/content_reveal_button");
            var reveal = new Button
            {
                Content = label,
                HorizontalAlignment = HorizontalAlignment.Left,
                Padding = new Thickness(8, 2, 8, 2),
                FontSize = 12,
            };
            AutomationProperties.SetAutomationId(reveal, Ids.ContentPolicyCollapsedRevealButton);
            AutomationProperties.SetName(reveal, label);
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
