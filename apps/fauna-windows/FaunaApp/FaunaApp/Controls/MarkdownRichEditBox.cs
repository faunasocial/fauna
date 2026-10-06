using System;
using System.Collections.Generic;
using System.Threading;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation.Peers;
using Microsoft.UI.Xaml.Automation.Provider;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Input;
using Windows.System;

namespace FaunaApp.Controls;

/// <summary>
/// A <see cref="RichEditBox"/> used as an in-place markdown <b>decoration surface</b>
/// (NOT a WYSIWYG editor): the document always holds the <i>literal markdown source</i>
/// (markers present), and <see cref="GetPlainText"/> returns that literal string — so a
/// send transmits exactly what was typed, with no rich-text tree and no
/// serialize-on-send boundary (docs/goal/ui/conversations.md § Compose-field inline
/// markdown styling / § Why inline-styling, not WYSIWYG). Source ranges are *visually*
/// styled via <c>ITextRange.CharacterFormat</c> by the host (<see cref="DmComposeBar"/>);
/// this control owns only the literal-text + automation contract.
///
/// WinUI's stock RichEditBox automation peer exposes TextPattern but <b>not</b>
/// IValueProvider, while the cross-app e2e bridge drives <c>dm-text-field</c> purely
/// through ValuePattern (SetValue to type, Value to read — see
/// <c>tests/e2e-unified/flaui-bridge/{Actions,ElementFinder}.cs</c>). This subclass
/// attaches <see cref="MarkdownRichEditBoxAutomationPeer"/>, which re-adds IValueProvider
/// over the document, so the value contract keeps working with no bridge change and no
/// flaky physical-typing fallback. It is the windows twin of the native decoration
/// surfaces the other apps wrap (linux GtkTextView, apple NSTextView).
/// </summary>
public partial class MarkdownRichEditBox : RichEditBox, IMarkdownEditTarget
{
    public MarkdownRichEditBox()
    {
        // This is a LITERAL-markdown-source surface, NOT a rich-text editor. A stock
        // RichEditBox lets the user apply real character formatting — via Ctrl+B/I/U and
        // the floating selection mini-toolbar — which mutates the document's CharacterFormat
        // WITHOUT inserting the corresponding `**`/`_` markers. That formatting is invisible
        // to GetPlainText (the literal source), so it is silently dropped on send (the
        // message goes out as plain text). Disable every built-in rich-formatting affordance
        // so the ONLY way to format is the shared markdown toolbar (which splices literal
        // markers): no Ctrl+B/I/U accelerators, and no selection formatting flyout.
        DisabledFormattingAccelerators = DisabledFormattingAccelerators.All;
        SelectionFlyout = null;

        // Single-line compose field (the prior TextBox set AcceptsReturn="False"):
        // swallow Enter so it neither inserts a paragraph nor commits a stray newline.
        KeyDown += (_, e) =>
        {
            if (e.Key == VirtualKey.Enter)
                e.Handled = true;
        };
    }

    /// <summary>The literal document text (markdown markers present), with the trailing
    /// paragraph CR the RichEdit document appends stripped. This is the string that is
    /// drafted, copied, and sent. RichEdit ends every paragraph with a bare <c>\r</c>; each is
    /// handed out as <c>\n</c>, the line ending every other app's field holds and the one the
    /// shared <c>decoration_map</c> splits lines on — without it no line after the first ever
    /// got its heading or quote styling, and a multi-line message went out with bare-CR line
    /// breaks. The swap is one character for one, so every offset still names the same
    /// RichEdit position.</summary>
    public string GetPlainText()
    {
        Document.GetText(TextGetOptions.None, out var s);
        return s.TrimEnd('\r').Replace('\r', '\n');
    }

    /// <summary>Replace the document with <paramref name="value"/> as literal text (no
    /// rich-text parsing). Used by the value peer (e2e SetValue), the page's MessageText
    /// setter (draft restore / clear-after-send), and the toolbar wrap.</summary>
    public void SetPlainText(string value)
    {
        Document.SetText(TextSetOptions.None, value ?? string.Empty);
        ContentVersion++;
    }

    /// <summary>Bumped by every <see cref="SetPlainText"/> — which also wipes the document's
    /// formatting even when the text is unchanged — so the decoration pass can tell a document
    /// it must restyle from one it already styled.</summary>
    public int ContentVersion { get; private set; }

    /// <summary>Collapse the caret to the end of the document — the position a real user leaves it
    /// in after typing. <see cref="SetPlainText"/> (Document.SetText) resets the selection to 0; the
    /// e2e value peer calls this after SetValue so the hide-mode caret-edge reveal matches web/linux
    /// (whose clear_and_type leaves the caret at the end). Without it a leading inline marker would
    /// sit at the caret edge and reveal instead of conceal (compose_decoration_plan reveals the run
    /// the caret is within, inclusive of its edges).</summary>
    public void MoveCaretToEnd()
    {
        int end = GetPlainText().Length;
        Document.Selection.SetRange(end, end);
    }

    /// <summary>Fixed-pitch faces this app paints, reported as the generic <c>monospace</c>
    /// family by <see cref="TextRuns"/> — apple's <c>isFixedPitch</c> classification, which
    /// TOM cannot answer from a face name alone.</summary>
    private static readonly HashSet<string> MonospaceFaces = new(StringComparer.OrdinalIgnoreCase)
    {
        "Consolas", "Cascadia Mono", "Cascadia Code", "Courier New", "Lucida Console",
    };

    /// <summary>The styling this field APPLIED, read off the live RichEdit document — never
    /// recomputed from the shared decoration plan: one record per character-format run
    /// (split again at paragraph breaks), its text and one look listing what the run SETS
    /// against the field defaults (<c>weight</c> 700 for bold, <c>family</c>
    /// <c>monospace</c> for a fixed-pitch face, <c>scale</c> against
    /// <paramref name="basePoints"/>, <c>left_margin</c> for a paragraph indent in points,
    /// <c>invisible</c> for a concealed marker; a default reads <c>null</c>). The shape of
    /// linux's <c>get_attr(dm-text-field, "text-runs")</c> (its <c>gtk::TextBuffer</c> read)
    /// and apple's <c>NSTextStorage</c> read; the windows e2e reaches it through the
    /// TestAgent's <c>compose_text_runs</c> command.</summary>
    public List<Dictionary<string, object?>> TextRuns(float basePoints, string defaultFace)
    {
        var runs = new List<Dictionary<string, object?>>();
        Document.GetText(TextGetOptions.None, out var text);
        var len = text.TrimEnd('\r').Length;
        var pos = 0;
        while (pos < len)
        {
            var probe = Document.GetRange(pos, pos + 1);
            probe.Expand(TextRangeUnit.CharacterFormat);
            var end = Math.Min(Math.Max(probe.EndPosition, pos + 1), len);
            var br = text.IndexOf('\r', pos, end - pos);
            if (br == pos) { pos++; continue; }   // a paragraph break is no run
            if (br > pos) end = br;

            var range = Document.GetRange(pos, end);
            var cf = range.CharacterFormat;
            var pf = range.ParagraphFormat;
            var face = cf.Name;
            var family = string.Equals(face, defaultFace, StringComparison.OrdinalIgnoreCase)
                ? null
                : MonospaceFaces.Contains(face) ? "monospace" : face;
            var scale = basePoints > 0 ? cf.Size / basePoints : 1f;
            runs.Add(new Dictionary<string, object?>
            {
                ["text"] = text.Substring(pos, end - pos),
                ["tags"] = new[]
                {
                    new Dictionary<string, object?>
                    {
                        ["name"] = null,
                        ["weight"] = cf.Bold == FormatEffect.On ? 700 : null,
                        ["family"] = family,
                        ["scale"] = Math.Abs(scale - 1f) > 0.01f ? Math.Round(scale, 3) : null,
                        ["left_margin"] = pf.LeftIndent > 0 ? Math.Round(pf.LeftIndent, 2) : null,
                        ["invisible"] = cf.Hidden == FormatEffect.On,
                    },
                },
            });
            pos = end;
        }
        return runs;
    }

    protected override AutomationPeer OnCreateAutomationPeer()
        => new MarkdownRichEditBoxAutomationPeer(this);

    // ── IMarkdownEditTarget (markdown toolbar wrap; conversations.md constraint) ──
    string IMarkdownEditTarget.Text
    {
        get => GetPlainText();
        set => SetPlainText(value);
    }

    int IMarkdownEditTarget.SelectionStart
    {
        get => Document.Selection.StartPosition;
        // Collapse the caret to `value` (a zero-length selection there) rather than
        // SetRange(value, EndPosition): the toolbar sets Text first (which resets the
        // selection to 0), THEN SelectionStart, THEN SelectionLength — so EndPosition is
        // stale (0) here, and SetRange(value, 0) would normalize to (0, value), leaving the
        // following SelectionLength to anchor at 0 and select the FIRST word. Collapsing
        // makes the subsequent SelectionLength extend from `value` (the correct anchor).
        set => Document.Selection.SetRange(value, value);
    }

    int IMarkdownEditTarget.SelectionLength
    {
        get => Document.Selection.EndPosition - Document.Selection.StartPosition;
        set => Document.Selection.SetRange(
            Document.Selection.StartPosition, Document.Selection.StartPosition + value);
    }

    void IMarkdownEditTarget.FocusTextInput() => Focus(FocusState.Programmatic);
}

/// <summary>
/// Custom automation peer that adds <see cref="IValueProvider"/> over the RichEditBox
/// document — the pattern the e2e bridge needs and the stock peer omits. Provider calls
/// are marshalled to the UI thread defensively: WinUI already dispatches UIA pattern
/// calls onto it, but a direct off-thread <c>Document</c> access would throw a
/// COMException (the same wrong-thread hazard as <c>ConfigureAwait(false)</c> in a WinUI
/// VM — memory <c>reference_windows_vm_configureawait_comexception</c>).
/// </summary>
internal sealed class MarkdownRichEditBoxAutomationPeer
    : FrameworkElementAutomationPeer, IValueProvider
{
    private readonly MarkdownRichEditBox _owner;

    public MarkdownRichEditBoxAutomationPeer(MarkdownRichEditBox owner) : base(owner)
        => _owner = owner;

    protected override object GetPatternCore(PatternInterface patternInterface)
        => patternInterface == PatternInterface.Value
            ? this
            : base.GetPatternCore(patternInterface);

    protected override AutomationControlType GetAutomationControlTypeCore()
        => AutomationControlType.Edit;

    protected override string GetClassNameCore() => nameof(MarkdownRichEditBox);

    public bool IsReadOnly => false;

    public string Value => RunOnUi(() => _owner.GetPlainText());

    public void SetValue(string value) => RunOnUi(() =>
    {
        _owner.SetPlainText(value);
        // SetText resets the caret to 0; leave it at the end (where a user who just typed `value`
        // would have it) so the hide-mode caret-edge reveal matches web/linux for the e2e.
        _owner.MoveCaretToEnd();
        return true;
    });

    /// <summary>Run <paramref name="fn"/> on the owner's UI thread and return its result,
    /// blocking the calling (UIA) thread until it completes. A no-op marshal when already
    /// on the UI thread.</summary>
    private T RunOnUi<T>(Func<T> fn)
    {
        var dq = _owner.DispatcherQueue;
        if (dq is null || dq.HasThreadAccess)
            return fn();

        T result = default!;
        using var done = new ManualResetEventSlim(false);
        dq.TryEnqueue(() =>
        {
            try { result = fn(); }
            catch { /* surfaced as default; UIA call must not crash the app */ }
            finally { done.Set(); }
        });
        done.Wait(2000);
        return result;
    }
}
