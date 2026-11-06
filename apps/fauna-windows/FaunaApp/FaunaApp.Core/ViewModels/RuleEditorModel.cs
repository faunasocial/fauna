using System.Globalization;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Services;
using uniffi.fauna_client_policy;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// The shared <c>rule-editor</c> component's state on windows (<c>nest-rules.md</c>
/// § The shared rule editor → <i>Three mountings, one shape</i>) — the twin of tui's
/// <c>rule_editor::Mount</c>, android's <c>RuleEditor</c> composable state and apple's
/// <c>RuleEditorPanel</c> model. One per mounting: the admin's in
/// <c>admin-nest-rules-section</c>, the guardian's in <c>family-policy-rules-section</c>.
///
/// <para><b>This holds and forwards; it decides nothing.</b> The rule list, the kinds,
/// the categories, the verdicts, the level scale, every label and defect sentence and
/// whether the document may be saved are <see cref="FfiRuleEditor"/>'s (shared Rust,
/// <c>fauna_client_policy::RuleEditor</c>), so the editor cannot differ between hosts
/// or across the 7 apps. What is left here is what the design lets diverge: the
/// keywords inputs as typed, the host's status line and the paint's shape
/// (<see cref="Paint"/>), which <c>Controls/RuleEditorPanel</c> turns into WinUI
/// controls.</para>
///
/// <para>Every picker's option IS the label the crate painted (the cross-app select
/// contract), so a pick maps back through the view's own lists by position. Every edit
/// answers whether it changed the document; an edit that changed nothing (a picker
/// re-raising its own value, a <c>TextChanged</c> echoing the text it was painted
/// with) keeps the host's status, so a fresh "Saved" is never cleared by the
/// repaint that follows it.</para>
/// </summary>
internal sealed class RuleEditorModel
{
    // The keywords inputs as typed: a single-line input re-painted from the stored
    // terms would eat a trailing comma mid-typing, so the typed text is kept per row
    // while the editor holds the parsed terms (tui's and android's twin).
    private readonly Dictionary<int, string> _typedKeywords = new();

    /// <summary>The working document; <c>null</c> until the host's read lands.</summary>
    internal FfiRuleEditor? Editor { get; private set; }

    /// <summary>The publisher's subscribed labelers a rule may name, as hex ids
    /// (<c>FfiRuleLabelersClient.RuleLabelers</c>), read beside the document.</summary>
    internal IReadOnlyList<string> Subscribed { get; set; } = Array.Empty<string>();

    /// <summary><c>rule-editor-status</c>'s text — the host's last verdict, or
    /// <c>null</c>; with none, the status shows the editor's own defects.</summary>
    internal string? Status { get; set; }

    /// <summary>A save is in flight: the save button greys and reads "Saving…".</summary>
    internal bool Saving { get; set; }

    /// <summary>Replace the working document — a fresh read or a landed save
    /// (<c>null</c>: back to the loading state). The host's status is cleared; the
    /// host sets its own verdict after.</summary>
    internal void Load(FfiRuleEditor? editor)
    {
        Editor = editor;
        _typedKeywords.Clear();
        Status = null;
    }

    /// <summary>Whether any rule is a labeler rule — the one paint the subscribed
    /// labelers' late arrival changes.</summary>
    internal bool HasLabelerRow =>
        Editor?.View().rows.Any(r => r.kind == RuleKind.Labeler) ?? false;

    // ── Edits ────────────────────────────────────────────────────────────

    /// <summary><c>rule-editor-add-button</c> — a category rule, the kind tui,
    /// android and apple add.</summary>
    internal bool AddRule()
    {
        if (Editor?.AddRule(RuleKind.Category) is null) return false;
        return Touched();
    }

    /// <summary><c>rule-editor-item-remove-button[row]</c>.</summary>
    internal bool RemoveRule(int row)
    {
        if (Editor is not { } editor || !editor.RemoveRule((uint)row)) return false;
        // Rows after this one shift up; their typed text is re-read from the
        // stored terms.
        _typedKeywords.Clear();
        return Touched();
    }

    /// <summary><c>rule-editor-item-kind-select[row]</c> — option
    /// <paramref name="option"/> of the view's <c>kinds</c>.</summary>
    internal bool PickKind(int row, int option)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        var kinds = editor.View().kinds;
        if (option < 0 || option >= kinds.Length || kinds[option].kind == r.kind) return false;
        if (!editor.SetKind((uint)row, kinds[option].kind)) return false;
        _typedKeywords.Remove(row);
        return Touched();
    }

    /// <summary><c>rule-editor-item-category-select[row]</c>.</summary>
    internal bool PickCategory(int row, int option)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        var categories = editor.View().categories;
        if (option < 0 || option >= categories.Length || categories[option].category == r.category) return false;
        return editor.SetCategory((uint)row, categories[option].category) && Touched();
    }

    /// <summary><c>rule-editor-item-verdict-select[row]</c>.</summary>
    internal bool PickVerdict(int row, int option)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        var verdicts = editor.View().verdicts;
        if (option < 0 || option >= verdicts.Length || verdicts[option].verdict == r.verdict) return false;
        return editor.SetVerdict((uint)row, verdicts[option].verdict) && Touched();
    }

    /// <summary><c>rule-editor-item-level-select[row]</c> — an index into the
    /// view's <c>levels</c>.</summary>
    internal bool PickLevel(int row, int option)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        var levels = editor.View().levels;
        if (option < 0 || option >= levels.Length || r.levelIndex == (uint)option) return false;
        return editor.SetLevel((uint)row, (uint)option) && Touched();
    }

    /// <summary>
    /// <c>rule-editor-item-labeler-select[row]</c> — option <paramref name="option"/>
    /// of <c>LabelerChoices</c>. A subscribed labeler the document does not bundle yet
    /// is fetched (<paramref name="loadModule"/>, the labeler's id as bytes) and handed
    /// over under the choice's name; a bundled one is chosen as it stands. A module that
    /// cannot be read leaves the rule without a labeler, whose defect the status names.
    /// </summary>
    internal async Task<bool> PickLabelerAsync(int row, int option, Func<byte[], Task<byte[]?>> loadModule)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        var choices = editor.LabelerChoices(Subscribed.ToArray());
        if (option < 0 || option >= choices.Length) return false;
        var choice = choices[option];
        if (choice.name == r.labeler) return false;
        if (choice.labelerId is not { } hex)
            return editor.ChooseBundledLabeler((uint)row, choice.name) && Touched();

        byte[]? module;
        try
        {
            module = await loadModule(Convert.FromHexString(hex));
        }
        catch (Exception ex)
        {
            ShellLog.Warn("RuleEditor", $"labeler module unavailable: {ex.GetType().Name}: {ex.Message}");
            module = null;
        }
        // The host may have reloaded the editor while the module was read.
        if (module is null || !ReferenceEquals(editor, Editor)) return false;
        return editor.SetLabeler((uint)row, choice.name, module) && Touched();
    }

    /// <summary><c>rule-editor-item-keywords-input[row]</c>, as the single-line input
    /// hands it (commas separate terms).</summary>
    internal bool SetKeywords(int row, string text)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        if (text == KeywordsText(row, r)) return false;
        if (!editor.SetKeywordsInline((uint)row, text)) return false;
        _typedKeywords[row] = text;
        return Touched();
    }

    /// <summary><c>rule-editor-item-prompt-input[row]</c>.</summary>
    internal bool SetPrompt(int row, string text)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        if (text == (r.prompt ?? "")) return false;
        return editor.SetPrompt((uint)row, text) && Touched();
    }

    /// <summary><c>rule-editor-item-reason-input[row]</c> — the required
    /// default-language reason (the view's <c>reasonDefaultKey</c>).</summary>
    internal bool SetReason(int row, string text)
    {
        if (Editor is not { } editor || RowAt(editor, row) is not { } r) return false;
        var key = editor.View().reasonDefaultKey;
        if (text == ReasonText(r, key)) return false;
        return editor.SetReason((uint)row, key, text) && Touched();
    }

    /// <summary>Any edit that changed the document drops the host's last verdict, so
    /// a stale "Saved" never sits over unsaved edits (tui's <c>status = None</c>).</summary>
    private bool Touched()
    {
        Status = null;
        return true;
    }

    private static RuleRow? RowAt(FfiRuleEditor editor, int row)
    {
        var rows = editor.View().rows;
        return row >= 0 && row < rows.Length ? rows[row] : null;
    }

    private string KeywordsText(int row, RuleRow r) =>
        _typedKeywords.TryGetValue(row, out var typed)
            ? typed
            : FaunaFfiMethods.RuleKeywordsInline(r.keywords ?? "");

    private static string ReasonText(RuleRow r, string key) =>
        r.reasons.FirstOrDefault(e => e.lang == key)?.text ?? "";

    // ── Paint ────────────────────────────────────────────────────────────

    /// <summary>Everything the panel paints, every string already resolved.</summary>
    internal RuleEditorPaint Paint()
    {
        if (Editor is not { } editor)
        {
            // A mounting whose read has not landed paints only its status (tui's
            // unloaded mount): the add button's absence is what `wait_loaded` waits on.
            var waiting = string.IsNullOrEmpty(Status) ? Strings.Get("rule_editor/loading") : Status;
            return new RuleEditorPaint(
                Loaded: false, Editable: false, Rows: Array.Empty<RuleRowPaint>(),
                AddEnabled: false, SaveEnabled: false, SaveLabel: Strings.Get("rule_editor/save_button"),
                Status: waiting);
        }

        var view = editor.View();
        var kinds = view.kinds.Select(k => Strings.Resolve(k.label)).ToArray();
        var categories = view.categories.Select(c => Strings.Resolve(c.label)).ToArray();
        var verdicts = view.verdicts.Select(v => Strings.Resolve(v.label)).ToArray();
        var levels = view.levels.Select(l => Strings.Resolve(l.label)).ToArray();
        var labelers = editor.LabelerChoices(Subscribed.ToArray()).Select(c => c.name).ToArray();

        var rows = new List<RuleRowPaint>(view.rows.Length);
        for (var i = 0; i < view.rows.Length; i++)
        {
            var row = view.rows[i];
            var subject = row.kind switch
            {
                RuleKind.Category => new RuleSubjectPaint(
                    RuleSubject.Category, categories,
                    Array.FindIndex(view.categories, c => c.category == row.category), ""),
                RuleKind.Keywords => new RuleSubjectPaint(
                    RuleSubject.Keywords, Array.Empty<string>(), -1, KeywordsText(i, row)),
                RuleKind.Labeler => new RuleSubjectPaint(
                    RuleSubject.Labeler, labelers, Array.IndexOf(labelers, row.labeler ?? ""), ""),
                RuleKind.Prompt => new RuleSubjectPaint(
                    RuleSubject.Prompt, Array.Empty<string>(), -1, row.prompt ?? ""),
                // A loaded region list rule: shown, removable, never edited.
                _ => new RuleSubjectPaint(RuleSubject.None, Array.Empty<string>(), -1, ""),
            };
            var hasLevel = row.levelIndex is { } li && li < (uint)levels.Length;
            rows.Add(new RuleRowPaint(
                Index: i,
                Title: Strings.Resolve(new LocalizedText(
                    "rule_editor.item_label",
                    new Dictionary<string, string> { ["rule"] = row.number.ToString(CultureInfo.InvariantCulture) })),
                ReadOnly: row.readOnly,
                Live: view.editable && !row.readOnly,
                KindLabel: Strings.Resolve(row.kindLabel),
                KindOptions: kinds,
                KindSelected: Array.FindIndex(view.kinds, k => k.kind == row.kind),
                Subject: subject,
                VerdictOptions: verdicts,
                VerdictSelected: Array.FindIndex(view.verdicts, v => v.verdict == row.verdict),
                LevelOptions: hasLevel ? levels : null,
                LevelSelected: hasLevel ? (int)row.levelIndex!.Value : -1,
                Reason: ReasonText(row, view.reasonDefaultKey),
                Inert: row.inert is { } inert ? Strings.Resolve(inert) : null,
                RemoveEnabled: view.editable));
        }

        // The host's verdict wins; before one, what stops a save — every defect
        // sentence, as android and apple paint them.
        var status = string.IsNullOrEmpty(Status)
            ? string.Join(" ", view.defects.Select(d => Strings.Resolve(d)))
            : Status;
        return new RuleEditorPaint(
            Loaded: true,
            Editable: view.editable,
            Rows: rows,
            AddEnabled: view.editable,
            SaveEnabled: view.canSave && !Saving,
            SaveLabel: Strings.Get(Saving ? "rule_editor/saving" : "rule_editor/save_button"),
            Status: status);
    }
}

/// <summary>A row's one subject control, by kind.</summary>
internal enum RuleSubject { None, Category, Keywords, Labeler, Prompt }

/// <summary>The row's subject control: a picker's options and selection, or an
/// input's text.</summary>
internal sealed record RuleSubjectPaint(
    RuleSubject Kind, IReadOnlyList<string> Options, int Selected, string Text);

/// <summary>One <c>rule-editor-item</c>, every label resolved. <see cref="LevelOptions"/>
/// is <c>null</c> when the rule has no level (a keywords rule) — the level select is
/// then absent, not disabled.</summary>
internal sealed record RuleRowPaint(
    int Index,
    string Title,
    bool ReadOnly,
    bool Live,
    string KindLabel,
    IReadOnlyList<string> KindOptions,
    int KindSelected,
    RuleSubjectPaint Subject,
    IReadOnlyList<string> VerdictOptions,
    int VerdictSelected,
    IReadOnlyList<string>? LevelOptions,
    int LevelSelected,
    string Reason,
    string? Inert,
    bool RemoveEnabled);

/// <summary>The whole <c>rule-editor</c> paint. <see cref="Status"/> is
/// <c>rule-editor-status</c>'s text (empty: the element is absent).</summary>
internal sealed record RuleEditorPaint(
    bool Loaded,
    bool Editable,
    IReadOnlyList<RuleRowPaint> Rows,
    bool AddEnabled,
    bool SaveEnabled,
    string SaveLabel,
    string Status);

/// <summary>What a host's save answers: the status line's text, and — for the admin,
/// whose reply carries the stored document — the editor to reload from it.</summary>
internal sealed record RuleEditorSaveOutcome(string? Status, FfiRuleEditor? Reload = null);

/// <summary>
/// The admin mounting's reads and write (<c>admin-nest-rules-section</c>,
/// <c>admin.md</c> § N. Nest → <i>Nest rules</i>): read on entry over
/// <c>fauna.admin.content_policy.get</c> beside the self-description's domain, written
/// through <c>.set</c>. android's <c>AdminNestRulesSection</c>, apple's and tui's
/// <c>apply_rules</c> / <c>apply_rules_saved</c> are the siblings. A failure lands on
/// the editor's status line, never blanking the rest of the page.
/// </summary>
internal static class RuleEditorHosts
{
    /// <summary>The admin's editor over the session nest's rules, or the status line
    /// saying why there is none.</summary>
    internal static async Task<(FfiRuleEditor? Editor, string? Domain, string? Status)> OpenNestAsync(INestRpcClient rpc)
    {
        try
        {
            var policy = await rpc.AdminContentPolicyAsync();
            var domain = await NestRulesHost.SessionDomainAsync(rpc);
            if (domain is null) return (null, null, "the nest's domain is not known yet");
            return (FfiRuleEditor.ForNest(domain, policy.document), domain, null);
        }
        catch (Exception ex)
        {
            return (null, null, Strings.Error(ex));
        }
    }

    /// <summary>Write the admin's rules: the stored record re-seeds the editor and the
    /// status names the version in force; a refusal names its defect.</summary>
    internal static async Task<RuleEditorSaveOutcome> SaveNestAsync(INestRpcClient rpc, string domain, byte[] document)
    {
        FfiAdminContentPolicy reply;
        try
        {
            reply = await rpc.AdminSetContentPolicyAsync(document);
        }
        catch (Exception ex)
        {
            return new RuleEditorSaveOutcome(Strings.Error(ex));
        }
        if (reply.defect is { } defect) return new RuleEditorSaveOutcome(defect);
        if (reply.document is not { } stored) return new RuleEditorSaveOutcome(Strings.Get("rule_editor/saved"));
        FfiRuleEditor? reload = null;
        try
        {
            reload = FfiRuleEditor.ForNest(domain, stored);
        }
        catch (Exception ex)
        {
            ShellLog.Warn("RuleEditor", $"stored rules unreadable: {ex.GetType().Name}: {ex.Message}");
        }
        return new RuleEditorSaveOutcome(SavedVersion(reply.sequence), reload);
    }

    /// <summary><c>rule_editor.saved_version</c> — "Saved — version {sequence}".</summary>
    internal static string SavedVersion(ulong sequence) =>
        Strings.Resolve(new LocalizedText(
            "rule_editor.saved_version",
            new Dictionary<string, string> { ["sequence"] = sequence.ToString(CultureInfo.InvariantCulture) }));

    /// <summary>The publisher's subscribed labelers — read beside each mounting's
    /// document. A failed read lists none; the picker still offers the document's own
    /// labelers.</summary>
    internal static async Task<IReadOnlyList<string>> SubscribedLabelersAsync(INestRpcClient rpc)
    {
        try
        {
            return await rpc.RuleLabelersAsync();
        }
        catch (Exception ex)
        {
            ShellLog.Warn("RuleEditor", $"subscribed labelers unavailable: {ex.GetType().Name}: {ex.Message}");
            return Array.Empty<string>();
        }
    }
}
