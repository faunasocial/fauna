using System;
using System.Collections.Generic;
using FaunaApp.Core.Markdown;
using Microsoft.UI.Text;
using Microsoft.UI.Xaml.Media;
using uniffi.fauna_ffi;
using Color = Windows.UI.Color;

namespace FaunaApp.Controls;

/// <summary>
/// Inline-markdown decoration applier for the compose field — the windows twin of linux
/// <c>compose_decoration.rs</c> (gtk::TextTag) and apple <c>MarkdownDecorator</c>
/// (textStorage). The <see cref="MarkdownRichEditBox"/> document holds the literal
/// markdown source; on every edit/caret-move this applies *visual* styling to source
/// ranges via <c>ITextRange.CharacterFormat</c> (content ranges styled; markers dimmed
/// off the caret's line, revealed on it), reusing the pure shared run-builder
/// <see cref="ComposeMarkdownDecorator.BuildRuns"/> over the shared
/// <c>fauna_core::markdown::decoration_map</c>. CharacterFormat changes are
/// formatting-only — the literal text and the ValuePattern contract are never touched
/// (docs/goal/ui/conversations.md § Compose-field inline markdown styling).
/// </summary>
public sealed partial class DmComposeBar
{
    // Re-enabled 2026-06-21 with the DEFERRED + BATCHED applier below. This applier was
    // DISABLED 2026-06-20 because running it SYNCHRONOUSLY inside the edit notification was
    // UNSOUND on WinUI 3 / ARM64 — mutating ITextRange.CharacterFormat re-entrantly while the
    // native RichEdit was mid-keystroke-insert drove:
    //   (a) a native busy-loop HANG while typing ("test " wedged the UI thread ~80% of a
    //       core, native frames only — confirmed via dotnet-stack); and
    //   (b) a WinRT E_BOUNDS (0x8000000b) fail-fast CRASH on the bold toolbar wrap
    //       (combase.dll → 0xc000027b in Microsoft.UI.Xaml.dll; a native fail-fast the
    //       decoration try/catch can't catch).
    // A real keystroke inserts incrementally and fires TextChanged MID-INSERT; a one-shot
    // SetValue rebuilds the whole document and fires TextChanged afterwards, so it never
    // reproduced — which is why both prior approaches (this + the 2026-06-19 overlay) shipped
    // behind a green SetValue test. The fix: (1) ScheduleDecoration DEFERS the pass off the
    // synchronous TextChanged/KeyUp/PointerReleased via DispatcherQueue (coalesced), so the
    // CharacterFormat mutation runs on a LATER turn — never re-entrantly inside the keystroke
    // insert or the toolbar's SetText; and (2) ApplyDecoration BATCHES the writes in
    // Document.BatchDisplayUpdates/ApplyDisplayUpdates so they coalesce into one re-layout.
    // A faithful per-keystroke repro is infeasible in the headless e2e harness (needs real key
    // input it can't inject); validated manually instead.
    // Kept as a kill-switch (flip to false to disable); static readonly, not const, so the
    // applier stays reachable to the compiler.
    private static readonly bool ComposeDecorationEnabled = true;

    private bool _decorationEnabled;
    private bool _applyingDecoration;

    // markdown-marker-toggle-button state: false = inline emphasis markers HIDDEN (the default —
    // concealed with caret-edge reveal via compose_decoration_plan); true = shown DIMMED (the
    // live-preview from decoration_map). Per-editor, client-local, NO persistence
    // (conversations.md § Compose-field inline markdown styling; the 2026-06-28 marker-default flip).
    private bool _markersShown;

    private Microsoft.UI.Dispatching.DispatcherQueueTimer? _decorationTimer;

    // What the last decoration pass styled — see ApplyDecoration's idempotency note.
    private (string, int, int, bool, bool, int)? _lastDecorationKey;

    private Color _defaultColor;
    private Color _secondaryColor;   // blockquote
    private Color _tertiaryColor;    // dimmed markers off the caret line
    private Color _accentColor;      // link
    private string _defaultFontName = "Segoe UI";
    private double _defaultFontSize = 14;

    /// <summary>Wire the re-decoration triggers and resolve theme colors once. Called
    /// from the constructor. RichEditBox has no SelectionChanged event, so caret-only
    /// moves (the per-line marker reveal depends on the caret line) are caught via
    /// KeyUp (arrow keys) and PointerReleased (clicks); typing fires TextChanged.</summary>
    private void InitializeDecoration()
    {
        // Kill-switch: when disabled, don't subscribe the per-edit re-decoration triggers,
        // so the field never mutates its document on TextChanged/KeyUp/PointerReleased.
        if (!ComposeDecorationEnabled) return;

        // Decoration defaults ON to match the always-shown markdown toolbar. SetCapabilities
        // adjusts it per rail for an open thread (the detail bar); the NEW-THREAD composer
        // never gets SetCapabilities (its rail isn't known until a recipient locks it — see
        // ConversationsPage.RefreshNewThreadView), so without this default it would never
        // style despite showing the toolbar.
        _decorationEnabled = true;

        _defaultColor = (MessageBox.Foreground as SolidColorBrush)?.Color
            ?? ThemeColor("TextFillColorPrimary", Microsoft.UI.Colors.Black);
        _secondaryColor = ThemeColor("TextFillColorSecondary", _defaultColor);
        _tertiaryColor = ThemeColor("TextFillColorTertiary", _defaultColor);
        _accentColor = ThemeColor("SystemAccentColor", _defaultColor);
        _defaultFontName = MessageBox.FontFamily?.Source ?? "Segoe UI";
        _defaultFontSize = MessageBox.FontSize > 0 ? MessageBox.FontSize : 14;

        // NB: ScheduleDecoration (NOT ApplyDecoration) — the styling pass must run on a
        // later dispatcher turn, never synchronously inside these notifications (see the
        // ComposeDecorationEnabled note: synchronous CharacterFormat mutation mid-keystroke
        // was the hang/crash).
        MessageBox.TextChanged += (_, _) => ScheduleDecoration();
        MessageBox.KeyUp += (_, _) => ScheduleDecoration();
        MessageBox.PointerReleased += (_, _) => ScheduleDecoration();
    }

    /// <summary>Gate styling on the rail's markdown capability (mirrors the toolbar
    /// gating). Disabling clears any applied styling back to plain.</summary>
    private void SetDecorationEnabled(bool enabled)
    {
        if (!ComposeDecorationEnabled) return;   // kill-switch (see ComposeDecorationEnabled note)
        _decorationEnabled = enabled;
        ApplyDecoration();
    }

    /// <summary>Flip this editor's inline-marker visibility — the markdown-marker-toggle-button.
    /// <c>false</c> (default) = markers HIDDEN (concealed + caret-edge reveal via
    /// <c>compose_decoration_plan</c>); <c>true</c> = markers shown DIMMED (the live-preview from
    /// <c>decoration_map</c>). Per-editor, no persistence (conversations.md § Compose-field inline
    /// markdown styling). Re-applies the decoration pass immediately so the flip is visible at once.
    /// Wired from <see cref="MarkdownToolbar.MarkersShownChanged"/> by the host constructor.</summary>
    public void SetMarkersShown(bool shown)
    {
        if (!ComposeDecorationEnabled) return;
        _markersShown = shown;
        ApplyDecoration();
    }

    /// <summary>(Re)start a short debounce so a single decoration pass runs ~150 ms after the
    /// user STOPS editing — never synchronously inside the keystroke. Two reasons:
    /// (1) the CharacterFormat mutation must not run re-entrantly inside the edit notification
    /// (TextChanged) or the toolbar's <c>Document.SetText</c> — doing so on WinUI 3 / ARM64
    /// drove the native RichEdit into a busy-loop hang and tripped an E_BOUNDS fail-fast on the
    /// bold wrap (see the <c>ComposeDecorationEnabled</c> note); and (2) re-formatting the
    /// whole range (re-shape + a shared-Rust <c>decoration_map</c> call) on EVERY keystroke
    /// makes fast multi-word typing visibly lag — debouncing collapses a burst of keystrokes
    /// into ONE pass during the natural pause, so no styling work happens while typing.
    /// The visual styling lags a fast typist by one short pause (imperceptible) and never
    /// touches the literal text or the ValuePattern contract.</summary>
    private void ScheduleDecoration()
    {
        if (!ComposeDecorationEnabled || !_decorationEnabled) return;
        var dq = MessageBox.DispatcherQueue;
        if (dq is null) return;
        if (_decorationTimer is null)
        {
            _decorationTimer = dq.CreateTimer();
            _decorationTimer.IsRepeating = false;
            _decorationTimer.Interval = TimeSpan.FromMilliseconds(150);
            _decorationTimer.Tick += (_, _) => ApplyDecoration();
        }
        // Restart on every edit: the pass fires only once typing has paused for the interval.
        _decorationTimer.Stop();
        _decorationTimer.Start();
    }

    /// <summary>Reset the document's formatting and (when enabled) re-apply the styled
    /// runs. UI-thread only (no ConfigureAwait — there is no async here; off-thread
    /// bound-state mutation throws COMException, memory
    /// <c>reference_windows_vm_configureawait_comexception</c>). Runs deferred + debounced via
    /// <see cref="ScheduleDecoration"/> for the per-edit triggers — never synchronously inside
    /// a keystroke insert, and at most once per typing pause — which is what stops the native
    /// re-layout storm (the per-keystroke synchronous version hung). Wrapped so a styling error
    /// can never break the field's literal text or its ValuePattern contract.</summary>
    private void ApplyDecoration()
    {
        if (!ComposeDecorationEnabled) return;
        if (_applyingDecoration) return;
        _applyingDecoration = true;
        var doc = MessageBox.Document;
        try
        {
            string text = MessageBox.GetPlainText();

            // Idempotent: a pass over the same text, caret, mode and content version as the last
            // one would only rewrite the same formats, and each rewrite is a burst of UIA
            // events that can itself schedule another pass. Without this the compose field's
            // UIA tree walk took 49 s in the caret witness (the bridge's SLOW FIND report,
            // dispatcher never stalled); with it that stall is gone. Nothing changed → nothing to do.
            var sel = doc.Selection;
            var key = (text, sel.StartPosition, sel.EndPosition, _decorationEnabled, _markersShown,
                MessageBox.ContentVersion);
            if (key == _lastDecorationKey) return;
            _lastDecorationKey = key;

            // Clear stale formatting from the previous edit first, so a removed marker
            // doesn't leave a dimmed/concealed glyph behind.
            ResetFormatting(text.Length);
            if (!_decorationEnabled || text.Length == 0)
            {
                // No styling/concealment applied → the visible text is the raw source.
                PublishVisibleText(text);
                return;
            }

            int caret = doc.Selection.StartPosition;
            IReadOnlyList<ComposeRun> runs;
            if (_markersShown)
            {
                // Show-markers mode: the shared show-markers dim set (markers off the caret line
                // dimmed, on it revealed) over the shared decoration_map. `caret` is a UTF-16 index;
                // the shared fn wants a byte offset (same as the hide-mode branch below).
                var dim = FaunaFfiMethods.ComposeShowMarkersDimRanges(
                    text, (ulong)ComposeMarkdownDecorator.Utf16IndexToUtf8Byte(text, caret));
                runs = ComposeMarkdownDecorator.BuildRuns(
                    text, FaunaFfiMethods.DecorationMap(text), dim);
            }
            else
            {
                // Hide-by-default mode: the shared plan decides which inline markers conceal vs
                // reveal (caret-edge); `caret` is a UTF-16 index, the plan wants a byte offset.
                var plan = FaunaFfiMethods.ComposeDecorationPlan(
                    text, (ulong)ComposeMarkdownDecorator.Utf16IndexToUtf8Byte(text, caret));
                runs = ComposeMarkdownDecorator.BuildHideRuns(
                    text, FaunaFfiMethods.DecorationMap(text), plan);
            }
            foreach (var run in runs)
                ApplyRun(text.Length, run);

            // Publish the rendered text (source minus the concealed runs) for the e2e
            // compose_visible_text read — see PublishVisibleText.
            PublishVisibleText(ComposeMarkdownDecorator.VisibleText(text, runs));
        }
        catch
        {
            // Decoration is a visual nicety; the literal source + ValuePattern contract
            // must hold even if styling throws. Swallow and leave the text plain.
        }
        finally
        {
            _applyingDecoration = false;
        }
    }

    /// <summary>Publish the compose field's rendered (visible) text on the RichEditBox's
    /// <c>AutomationProperties.HelpText</c> — the channel the cross-app e2e reads via
    /// <c>compose_visible_text</c> (<c>get_attr("dm-text-field","visible")</c> → HelpText; the
    /// bridge maps any non-name/disabled attr to HelpText). In hide mode the concealed inline
    /// markers are absent; in show mode it equals the source. The windows twin of web's
    /// concealed-excluding <c>textContent</c> and linux's <c>include_hidden_chars=false</c> read;
    /// the literal source (ValuePattern / send bytes) is untouched.</summary>
    private void PublishVisibleText(string visible) =>
        Microsoft.UI.Xaml.Automation.AutomationProperties.SetHelpText(MessageBox, visible ?? string.Empty);

    /// <summary>Convert a DIP/pixel size (WinUI <c>FontSize</c> unit, 96/inch) to typographic
    /// points (<c>ITextCharacterFormat.Size</c> unit, 72/inch). Without this the decoration
    /// renders every glyph 1.333× larger than the field's native <c>FontSize</c> — the "text
    /// jumps bigger once styling kicks in" artifact.</summary>
    private static float DipToPoints(double dip) => (float)(dip * 72.0 / 96.0);

    /// <summary>Reset the whole content range to the field defaults (clears any prior
    /// bold/italic/mono/heading/dim styling without touching the text).</summary>
    private void ResetFormatting(int len)
    {
        var all = MessageBox.Document.GetRange(0, len);
        var cf = all.CharacterFormat;
        cf.Bold = FormatEffect.Off;
        cf.Italic = FormatEffect.Off;
        cf.Hidden = FormatEffect.Off;   // un-conceal anything the previous hide-mode pass hid
        cf.Name = _defaultFontName;
        cf.Size = DipToPoints(_defaultFontSize);
        cf.ForegroundColor = _defaultColor;
        all.CharacterFormat = cf;   // set back — see ApplyRun
        var pf = all.ParagraphFormat;
        pf.SetIndents(0, 0, 0);     // un-indent a line that stopped being a quote
        all.ParagraphFormat = pf;
    }

    /// <summary>The compose quote indent, in points: 12px, the indent linux and apple give a
    /// quoted compose line (conversations.md § Compose-field inline markdown styling:
    /// "quote indent").</summary>
    private static readonly float QuoteIndentPoints = DipToPoints(12);

    /// <summary>This field's applied styling, read off its live document
    /// (<see cref="MarkdownRichEditBox.TextRuns"/>) against the defaults this pass resets to.</summary>
    internal List<Dictionary<string, object?>> ComposeTextRuns() =>
        MessageBox.TextRuns(DipToPoints(_defaultFontSize), _defaultFontName);

    /// <summary>Apply one styled run via CharacterFormat. Mirrors the render path
    /// (<c>DmMessageBubble</c>) and the old overlay's <c>BuildInline</c> so the compose
    /// preview reads like the sent message (priority #3); markers off the caret's line
    /// are dimmed (DimMarker), on it shown un-dimmed (Plain → default).</summary>
    private void ApplyRun(int textLen, ComposeRun run)
    {
        if (run.Style == ComposeRunStyle.Plain) return; // defaults already applied
        int end = Math.Min(run.Start + run.Length, textLen);
        if (end <= run.Start) return;
        var range = MessageBox.Document.GetRange(run.Start, end);
        var cf = range.CharacterFormat;
        switch (run.Style)
        {
            case ComposeRunStyle.Bold:
                cf.Bold = FormatEffect.On;
                break;
            case ComposeRunStyle.Italic:
                cf.Italic = FormatEffect.On;
                break;
            case ComposeRunStyle.BoldItalic:
                cf.Bold = FormatEffect.On;
                cf.Italic = FormatEffect.On;
                break;
            case ComposeRunStyle.Code:
                cf.Name = "Consolas";
                break;
            case ComposeRunStyle.Link:
                cf.ForegroundColor = _accentColor;
                break;
            case ComposeRunStyle.Heading:
                cf.Bold = FormatEffect.On;
                cf.Size = DipToPoints(HeadingFontSize(run.HeadingLevel));
                break;
            case ComposeRunStyle.Blockquote:
                cf.Italic = FormatEffect.On;
                cf.ForegroundColor = _secondaryColor;
                // The whole quoted line indents (QuoteIndentPoints).
                var pf = range.ParagraphFormat;
                pf.SetIndents(0, QuoteIndentPoints, 0);
                range.ParagraphFormat = pf;
                break;
            case ComposeRunStyle.DimMarker:
                cf.ForegroundColor = _tertiaryColor;
                break;
            case ComposeRunStyle.Conceal:
                // Hide-by-default: truly conceal the inline marker (zero-width, not rendered).
                // The literal char stays in the document (GetText / ValuePattern still return it),
                // so the source is sent verbatim — only the rendering hides it. RichEditBox has no
                // display:none; ITextCharacterFormat.Hidden is the TOM-backed true-hide that also
                // collapses the run's width (unlike a transparent foreground, which leaves a gap).
                cf.Hidden = FormatEffect.On;
                break;
        }
        // Set the format back onto the range — modifying the value returned by the
        // CharacterFormat getter does not necessarily apply to the underlying text.
        range.CharacterFormat = cf;
    }

    /// <summary>Heading point size — mirrors <c>DmMessageBubble.HeadingFontSize</c> so a
    /// heading reads identically in the editor and the sent bubble.</summary>
    private static double HeadingFontSize(int level) => level switch
    {
        1 => 20,
        2 => 17,
        3 => 15,
        _ => 14,
    };

    private static Color ThemeColor(string key, Color fallback) =>
        Microsoft.UI.Xaml.Application.Current.Resources.TryGetValue(key, out var v) && v is Color c
            ? c
            : fallback;
}
