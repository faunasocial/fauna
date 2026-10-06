using System;
using System.Collections.Generic;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using FaunaApp.Core.Helpers;
using FaunaApp.Helpers;
using uniffi.fauna_core;

namespace FaunaApp.Controls;

/// <summary>
/// The body of a shared semantic <c>RenderDocument</c> (render-model.md § The boundary), painted
/// as its line-run segments (<see cref="DocumentRenderer.Segments"/>; render-model.md
/// § Implementation status today, the windows *Text-block line runs* entry).
///
/// <para><b>One or many.</b> A body with no block over the shared <c>MAX_LINES_PER_TEXT_RUN</c>
/// budget — every ordinary message — is ONE <see cref="TextBlock"/>, painted from
/// <see cref="BodyPaint.Joined"/>, which is byte-identical to <c>DocumentPainter.Apply</c>'s
/// whole-document paint. A body with a split block is one <see cref="TextBlock"/> per segment
/// in an <see cref="ItemsRepeater"/> (<see cref="StackLayout"/>), which realizes only the
/// segments inside the enclosing <c>ScrollViewer</c>'s effective viewport. Why: a single fresh
/// layout of a multi-megabyte <see cref="TextBlock"/> in the live tree costs tens of seconds
/// (measured 2026-09-13, mail-message-size.md § Implementation status today), and WinUI's cost
/// is the text handed to ONE widget — so, unlike apple's break-count class, the projection's
/// value here is VIRTUALIZATION: a ~3 MiB plain-text mail lays out a screenful of 16-line runs,
/// never the whole body. The same one-or-many shape as apple's <c>LineRunsView</c>.</para>
///
/// <para><b>The automation read.</b> This control carries the body element id
/// (<c>dm-message-text</c>) and answers UIA's <see cref="IValueProvider"/> with the DOCUMENT's
/// plaintext (<c>render_document_to_plaintext</c> — the shared face apple's reads use), through
/// <see cref="DocumentBodyViewAutomationPeer"/>. The e2e bridge's <c>GetText</c> tries the Value
/// pattern before scraping descendant <c>TextBlock</c>s (<c>flaui-bridge/ElementFinder.cs</c>),
/// so the read is the document, never the realized widgets — which is what lets the paint be
/// split at all (render-model.md § Implementation status today, the *apple read-path
/// uniformity* bullet: "a windows change that splits the body across widgets owes the read
/// its own seam first"). <c>internal</c> setter because the UniFFI-generated
/// <see cref="RenderDocument"/> is emitted <c>internal</c>.</para>
/// </summary>
public sealed class DocumentBodyView : UserControl
{
    private RenderDocument? _document;
    // The plaintext of `_document`, derived on first read and dropped with the document. Only
    // the automation peer reads it.
    private string? _plaintext;
    private TextBlock? _single;
    private ItemsRepeater? _repeater;

    /// <summary>The document to paint. Setting the SAME instance again is a no-op (a page
    /// re-bind hands a fresh snapshot object per observer tick, so a repaint per tick is the
    /// norm — cheap for the one-widget case, and for the virtualized case only the on-screen
    /// segments re-lay out).</summary>
    internal RenderDocument? Document
    {
        get => _document;
        set
        {
            if (ReferenceEquals(_document, value)) return;
            _document = value;
            _plaintext = null;
            Paint();
        }
    }

    /// <summary>The document's plaintext for the automation read — the shared
    /// <c>render_document_to_plaintext</c>, memoized per document. Safe off the UI thread: it
    /// touches no XAML object, only the immutable snapshot record.</summary>
    internal string Plaintext
    {
        get
        {
            var doc = _document;
            if (doc is null) return string.Empty;
            return _plaintext ??= uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentToPlaintext(doc);
        }
    }

    private void Paint()
    {
        var doc = _document ?? new RenderDocument(Array.Empty<RenderBlock>());
        var paint = DocumentRenderer.Segments(doc);
        if (FaunaApp.Core.Logs.E2eTrace.Enabled)
            FaunaApp.Core.Logs.E2eTrace.Write($"[doc-body] segments={paint.Segments.Count} split={paint.Split}");

        if (!paint.Split)
        {
            _repeater = null;
            _single ??= NewTextBlock();
            DocumentPainter.ApplyRuns(_single, paint.Joined());
            if (!ReferenceEquals(Content, _single)) Content = _single;
            return;
        }

        _single = null;
        _repeater ??= new ItemsRepeater
        {
            ItemTemplate = new SegmentFactory(),
            Layout = new StackLayout(),
        };
        _repeater.ItemsSource = paint.Segments;
        if (!ReferenceEquals(Content, _repeater)) Content = _repeater;
    }

    // Built in code, never in XAML: a TextBlock with an x:Name and no explicit AutomationId gets
    // the x:Name AS its AutomationId (memory reference_winui_missing_automationid_falls_back_to_xname),
    // and the body id must sit on this control alone.
    private static TextBlock NewTextBlock() => new() { TextWrapping = TextWrapping.Wrap };

    protected override AutomationPeer OnCreateAutomationPeer()
        => new DocumentBodyViewAutomationPeer(this);

    /// <summary>One <see cref="TextBlock"/> per realized segment. No pooling: the repeater
    /// realizes a screenful at a time, and a fresh widget per realization keeps the recycle
    /// contract trivially correct.</summary>
    private sealed class SegmentFactory : IElementFactory
    {
        public UIElement GetElement(ElementFactoryGetArgs args)
        {
            var textBlock = NewTextBlock();
            if (args.Data is IReadOnlyList<MarkdownRun> runs)
                DocumentPainter.ApplyRuns(textBlock, runs);
            return textBlock;
        }

        public void RecycleElement(ElementFactoryRecycleArgs args) { }
    }
}

/// <summary>
/// Custom automation peer that adds <see cref="IValueProvider"/> over the painted document —
/// the pattern the e2e bridge reads first and the stock peer omits. The value is the
/// document's plaintext (<see cref="DocumentBodyView.Plaintext"/>), so it is the same text
/// however many widgets the body is split across and whether or not a segment is realized.
/// Prior art: <c>MarkdownRichEditBoxAutomationPeer</c>. No UI-thread marshal here, unlike that
/// peer: the value derives from the immutable snapshot record, not from any XAML object.
/// </summary>
internal sealed class DocumentBodyViewAutomationPeer
    : FrameworkElementAutomationPeer, IValueProvider
{
    private readonly DocumentBodyView _owner;

    public DocumentBodyViewAutomationPeer(DocumentBodyView owner) : base(owner)
        => _owner = owner;

    protected override object GetPatternCore(PatternInterface patternInterface)
        => patternInterface == PatternInterface.Value
            ? this
            : base.GetPatternCore(patternInterface);

    protected override AutomationControlType GetAutomationControlTypeCore()
        => AutomationControlType.Text;

    protected override string GetClassNameCore() => nameof(DocumentBodyView);

    public bool IsReadOnly => true;

    public string Value => _owner.Plaintext;

    public void SetValue(string value)
        => throw new InvalidOperationException("the document body is read-only");
}
