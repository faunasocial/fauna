using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Media.Imaging;
using uniffi.fauna_core;
using FaunaApp.UiIds;

namespace FaunaApp.Helpers;

/// <summary>
/// Paints a shared semantic <c>RenderDocument</c> body (render-model.md § The boundary)
/// into a WinUI <see cref="TextBlock"/>'s inline run, and its folded body remote images
/// (render-model.md § D3 + § Implementation status) into a host <see cref="Panel"/> as their
/// own <c>doc-remote-image</c> widgets. The document structure is decided ONCE in shared
/// Rust (<c>fauna_core::render</c>): the body walk goes through
/// <see cref="FaunaApp.Core.Helpers.DocumentRenderer.Flatten"/>, the remote-image list through
/// <see cref="FaunaApp.Core.Helpers.RemoteImageCardModel.All"/>. This is the single WinUI-side
/// mapper for both, so every surface that renders a body — the conversations
/// <c>DmMessageBubble</c> AND the feed post card / detail (render-model.md § D1/§ D6) — paints
/// identically with no duplicated markup logic (priority #1/#4). No client re-parses or
/// re-formats the body at render time.
/// </summary>
internal static class DocumentPainter
{
    /// <summary>Clear and repaint <paramref name="textBlock"/> from <paramref name="document"/>.
    /// A body remote image no longer paints here — <c>doc-remote-image</c> (ui.yaml, indexed,
    /// ID user-approved 2026-07-31) is its own element now, painted by <see cref="ApplyRemoteImages"/>
    /// instead (the same "own element ID ⇒ painted by the page" rule <c>post-image</c> /
    /// <c>quoted-post</c> / <c>link-preview-card</c> already follow), so the walker's
    /// <c>RemoteImage</c> arm is inert (<c>DocumentRenderer.Flatten</c> excludes it from the body).</summary>
    internal static void Apply(TextBlock textBlock, RenderDocument document)
        => ApplyRuns(textBlock, FaunaApp.Core.Helpers.DocumentRenderer.Flatten(document));

    /// <summary>Clear and repaint <paramref name="textBlock"/> from an already-walked run list —
    /// <see cref="Apply"/>'s body, split out so <c>DocumentBodyView</c> can paint ONE segment of
    /// <see cref="FaunaApp.Core.Helpers.DocumentRenderer.Segments"/> per <c>TextBlock</c> with the
    /// identical run→Inline mapping (render-model.md § Implementation status today, the windows
    /// line-run entry).</summary>
    internal static void ApplyRuns(TextBlock textBlock, IEnumerable<FaunaApp.Core.Helpers.MarkdownRun> runs)
    {
        textBlock.Inlines.Clear();
        foreach (var run in runs)
        {
            if (run.IsLineBreak)
            {
                textBlock.Inlines.Add(new Microsoft.UI.Xaml.Documents.LineBreak());
                continue;
            }

            // [label](url) → a real clickable Hyperlink (the old regex converter only
            // colored bare URLs); shared-parser links carry an absolute http(s) href.
            if (run.Link is { Length: > 0 } href)
            {
                var hyperlink = new Microsoft.UI.Xaml.Documents.Hyperlink();
                if (Uri.TryCreate(href, UriKind.Absolute, out var uri))
                    hyperlink.NavigateUri = uri;
                var linkRun = new Microsoft.UI.Xaml.Documents.Run { Text = run.Text };
                if (run.Bold) linkRun.FontWeight = Microsoft.UI.Text.FontWeights.Bold;
                if (run.Italic) linkRun.FontStyle = Windows.UI.Text.FontStyle.Italic;
                hyperlink.Inlines.Add(linkRun);
                textBlock.Inlines.Add(hyperlink);
                continue;
            }

            var textRun = new Microsoft.UI.Xaml.Documents.Run { Text = run.Text };
            if (run.Bold || run.HeadingLevel > 0)
                textRun.FontWeight = Microsoft.UI.Text.FontWeights.Bold;
            if (run.Italic)
                textRun.FontStyle = Windows.UI.Text.FontStyle.Italic;
            if (run.Monospace)
                textRun.FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Consolas");
            if (run.HeadingLevel > 0)
                textRun.FontSize = HeadingFontSize(run.HeadingLevel);
            textBlock.Inlines.Add(textRun);
        }
    }

    /// <summary>Clear and repaint <paramref name="host"/> with one <c>doc-remote-image</c>
    /// widget per <paramref name="images"/> entry, in body order — the feed post-card, the
    /// post_detail dialog, and the DM bubble all call this with the SAME
    /// <see cref="FaunaApp.Core.Helpers.RemoteImageCardModel"/> list so the three surfaces paint
    /// identically (priority #1/#2/#4). Registers in EVERY state under the one id — blocked
    /// placeholder or the loaded picture — so "has it painted?" is a question about the
    /// element's content, not its existence (render-model.md § D3; apps/tui.md § Rendering).</summary>
    internal static void ApplyRemoteImages(Panel host, IReadOnlyList<FaunaApp.Core.Helpers.RemoteImageCardModel> images)
    {
        host.Children.Clear();
        foreach (var image in images)
            host.Children.Add(BuildRemoteImage(image));
    }

    /// <summary>One <c>doc-remote-image</c> widget. When <c>card.Revealed</c> is false (the
    /// default — the manager has not yet revealed this block) it is a BLOCKED placeholder — a
    /// picture glyph + the alt text + a "remote image blocked" caption — with NO image source,
    /// so the framework issues no network request. When the manager has revealed it
    /// (<c>RenderBlock.RemoteImage.revealed==true</c>, projected by D3), it constructs a
    /// <see cref="BitmapImage"/> from the url — the one and only inbound remote fetch, gated on
    /// the user's explicit click dispatched to the manager (render-model.md § D3;
    /// html-mail.md § Security &amp; privacy). ONE Border carries the <c>doc-remote-image</c>
    /// AutomationId in both states — a bare AutomationId-only Border is UIA-pruned
    /// (reference_winui_flaui_datatemplate_name), so Name carries the alt text (falling back to
    /// the blocked caption when alt is empty) to stay discoverable either way.</summary>
    private static Border BuildRemoteImage(FaunaApp.Core.Helpers.RemoteImageCardModel card)
    {
        var border = new Border { HorizontalAlignment = HorizontalAlignment.Left, Margin = new Thickness(0, 4, 0, 0) };
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetAutomationId(border, Ids.DocRemoteImage);
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetName(border,
            string.IsNullOrEmpty(card.Alt)
                ? FaunaApp.Core.Services.Strings.Get("conversations/detail/remote_image_blocked")
                : card.Alt);

        if (card.Revealed && Uri.TryCreate(card.Url, UriKind.Absolute, out var uri))
        {
            border.Child = new Image
            {
                Source = new BitmapImage(uri),
                MaxHeight = 200,
                Stretch = Microsoft.UI.Xaml.Media.Stretch.Uniform,
                HorizontalAlignment = HorizontalAlignment.Left,
            };
            return border;
        }

        // Blocked placeholder (no Source ⇒ no request): 🖼 alt (Remote image blocked).
        var row = new StackPanel { Orientation = Orientation.Horizontal, Spacing = 4 };
        row.Children.Add(new TextBlock
        {
            Text = " ", // Segoe MDL2 "Photo2"
            FontFamily = new Microsoft.UI.Xaml.Media.FontFamily("Segoe MDL2 Assets"),
        });
        if (!string.IsNullOrEmpty(card.Alt))
            row.Children.Add(new TextBlock { Text = card.Alt });
        row.Children.Add(new TextBlock
        {
            Text = $"({FaunaApp.Core.Services.Strings.Get("conversations/detail/remote_image_blocked")})",
            FontStyle = Windows.UI.Text.FontStyle.Italic,
        });
        border.Child = row;
        return border;
    }

    // Body inherits the ~14px default; headings step up from it (h1 largest).
    private static double HeadingFontSize(int level) => level switch
    {
        1 => 20,
        2 => 17,
        3 => 15,
        _ => 14,
    };
}
