using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Everything one feature-limits authoring host paints
/// (<c>dynamic-features.md</c> § Authoring surfaces): the tier's authored rows,
/// the one open editor's view, and the host's last verdict line / error.
/// </summary>
internal sealed record FeatureAuthoringState(
    // null until the authored read answers — a summary painted before that would
    // say "No limit set" with no basis.
    IReadOnlyList<FfiAuthoredRow>? Rows,
    // The worded reason the authored read failed (the admin host paints it).
    string? ReadError,
    // The member the open editor is over, or null when none is open.
    string? OpenFeature,
    // The shared editor's derived view; set exactly while OpenFeature is.
    FfiPolicyEditorView? View,
    // `feature-policy-editor-status` — present only after a write.
    string? Status,
    // A refused cell or a failed write, worded — the page's `error-message`.
    string? Error)
{
    internal static FeatureAuthoringState Empty { get; } = new(null, null, null, null, null, null);
}

/// <summary>
/// ONE host's state machine for the shared <c>feature-policy-editor</c> family —
/// the C# shell around <c>FfiPolicyEditor</c> that the admin Nest page, the
/// Status sub-page's own-limit control and the Family page's guardian section
/// each hold, differing only in the two delegates that name their tier. The twin
/// of android's <c>FeatureAuthoringHost</c>, linux's <c>feature_editor.rs</c>
/// <c>HostState</c> and web's host wiring.
///
/// <para><b>This decides nothing.</b> The draft, the cell list and its order, the
/// labels, the no-effect notes, whether <i>Remove limit</i> renders and the parser
/// behind <i>Save</i> are all the shared editor's; this class holds the handle,
/// forwards keystrokes, and re-reads <see cref="IFfiPolicyEditor.View"/> after each
/// change. The editor is held as the interface so a unit test drives it without
/// the native library.</para>
///
/// <para>Methods are awaited on the caller's (UI) context — never
/// <c>ConfigureAwait(false)</c>: <see cref="Changed"/> repaints bound controls,
/// and off-thread mutation of them is a silent COMException (the windows VM
/// rule).</para>
/// </summary>
internal sealed class FeatureAuthoringHost
{
    private readonly Func<Task<IReadOnlyList<FfiAuthoredRow>>> _readRows;
    private readonly Func<string, Task<IFfiPolicyEditor>> _openEditor;
    private readonly Func<LocalizedText, string> _resolve;
    private readonly Func<Task>? _onWritten;

    private IFfiPolicyEditor? _editor;
    private bool _working;

    internal FeatureAuthoringHost(
        Func<Task<IReadOnlyList<FfiAuthoredRow>>> readRows,
        Func<string, Task<IFfiPolicyEditor>> openEditor,
        Func<LocalizedText, string>? resolve = null,
        // A write landed (the editor has already re-seeded): the owner re-reads
        // whatever else shows the same plane (the Status row's transparency cells).
        Func<Task>? onWritten = null)
    {
        _readRows = readRows;
        _openEditor = openEditor;
        _resolve = resolve ?? Strings.Resolve;
        _onWritten = onWritten;
    }

    internal FeatureAuthoringState State { get; private set; } = FeatureAuthoringState.Empty;

    /// <summary>The shape of the host changed (rows read, an editor opened / closed,
    /// the On/Off choice flipped, a verdict landed). NOT raised per keystroke —
    /// typing updates the cell notes in place so the entry keeps focus.</summary>
    internal event Action? Changed;

    /// <summary>The authored row for <paramref name="feature"/>, once the read has answered.</summary>
    internal FfiAuthoredRow? RowFor(string feature) =>
        State.Rows?.FirstOrDefault(r => r.@feature == feature);

    /// <summary>Read this tier's authored documents. An open editor is kept.</summary>
    internal async Task RefreshAsync()
    {
        try
        {
            var rows = await _readRows();
            State = State with { Rows = rows, ReadError = null };
        }
        catch (Exception e)
        {
            State = State with { Rows = null, ReadError = Failure("features.editor_load_failed", e) };
        }
        Changed?.Invoke();
    }

    /// <summary>Forget everything (the guardian host's ward changed): an editor is
    /// one ward's, and rows for another ward must not flash before the re-read.</summary>
    internal void Reset()
    {
        _editor = null;
        State = FeatureAuthoringState.Empty;
        Changed?.Invoke();
    }

    /// <summary>Open the editor over <paramref name="feature"/>, seeded from the
    /// tier's AUTHORED document.</summary>
    internal async Task OpenAsync(string feature)
    {
        try
        {
            var opened = await _openEditor(feature);
            _editor = opened;
            State = State with
            {
                OpenFeature = feature,
                View = opened.View(),
                Status = null,
                Error = null,
            };
        }
        catch (Exception e)
        {
            State = State with { Error = Failure("features.editor_load_failed", e) };
        }
        Changed?.Invoke();
    }

    /// <summary><c>feature-policy-editor-on-radio</c> (<c>true</c>) / <c>-off-radio</c>.</summary>
    internal void SetOn(bool on)
    {
        if (_editor is not { } editor) return;
        editor.SetOn(on);
        State = State with { View = editor.View() };
        Changed?.Invoke();
    }

    /// <summary><c>feature-policy-editor-cell-input[index]</c> — one keystroke's new
    /// text. Raises nothing; the caller repaints the notes from the returned view.</summary>
    internal FfiPolicyEditorView? SetCell(int index, string text)
    {
        if (_editor is not { } editor) return null;
        editor.SetCell((uint)index, text);
        var view = editor.View();
        State = State with { View = view };
        return view;
    }

    /// <summary><c>feature-policy-editor-save-button</c>.</summary>
    internal Task SaveAsync() => WriteAsync(remove: false);

    /// <summary><c>feature-policy-editor-remove-button</c>.</summary>
    internal Task RemoveAsync() => WriteAsync(remove: true);

    /// <summary><c>feature-policy-editor-cancel-button</c> — closes the editor,
    /// touching nothing.</summary>
    internal void Cancel()
    {
        _editor = null;
        State = State with { OpenFeature = null, View = null, Status = null, Error = null };
        Changed?.Invoke();
    }

    /// <summary>
    /// Save (or, with <paramref name="remove"/>, clear) the open editor's
    /// document. The shared parser decides: a cell it cannot read dispatches
    /// nothing and its reason lands on the page's error banner, exactly as on tui.
    /// </summary>
    private async Task WriteAsync(bool remove)
    {
        if (_editor is not { } editor || _working) return;
        _working = true;
        try
        {
            State = State with { Error = null };
            var verdict = remove ? await editor.Remove() : await editor.Save();
            if (verdict.@invalid is { } invalid)
            {
                State = State with { Error = _resolve(invalid) };
                Changed?.Invoke();
                return;
            }

            State = State with
            {
                View = editor.View(),
                Status = verdict.@status is { } status ? _resolve(status) : null,
            };
            Changed?.Invoke();
            await RefreshAsync();
            if (_onWritten is { } onWritten) await onWritten();
            Changed?.Invoke();
        }
        catch (Exception e)
        {
            State = State with { Error = Failure("features.editor_save_failed", e) };
            Changed?.Invoke();
        }
        finally
        {
            _working = false;
        }
    }

    private string Failure(string key, Exception e) =>
        _resolve(new LocalizedText(key, new Dictionary<string, string> { ["error"] = e.Message }));
}
