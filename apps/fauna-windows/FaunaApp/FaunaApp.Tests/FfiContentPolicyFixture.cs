using uniffi.fauna_client_policy;
using uniffi.fauna_ffi;

namespace FaunaApp.Tests;

/// <summary>
/// Test-only convenience: mint a guardian <see cref="FfiContentPolicy"/> — a rule
/// document and nothing else — with one category rule per named category, authored
/// through the shared rule editor exactly as a guardian's app authors one
/// (<c>FfiRuleEditor.ForGuardian(null)</c> → <c>AddRule(Category)</c> →
/// <c>SetCategory</c> → <c>SetVerdict</c> → <c>SetReason</c> →
/// <c>DocumentBytes()</c>), so no test hand-encodes the document. A rule sits at the
/// editor's default level. No category named → no document (a guardian who
/// authored nothing). The sibling of <see cref="PostSummaryFixture"/> for the
/// pattern.
/// </summary>
internal static class FfiContentPolicyFixture
{
    internal static FfiContentPolicy Make(
        RuleVerdict? nsfw = null,
        RuleVerdict? spam = null,
        RuleVerdict? phishing = null,
        RuleVerdict? commercial = null)
    {
        var rules = new (string Category, RuleVerdict? Verdict)[]
        {
            ("nsfw", nsfw), ("spam", spam), ("phishing", phishing), ("commercial", commercial),
        };
        if (rules.All(r => r.Verdict is null)) return new FfiContentPolicy(document: null);

        using var editor = FfiRuleEditor.ForGuardian(null);
        var reasonKey = editor.View().reasonDefaultKey;
        foreach (var (category, verdict) in rules)
        {
            if (verdict is not { } v) continue;
            var row = editor.AddRule(RuleKind.Category)
                ?? throw new InvalidOperationException("the guardian editor refused a category rule");
            // Setting a value the new rule already holds may answer false; the
            // document read back is what the fixture relies on.
            editor.SetCategory(row, category);
            editor.SetVerdict(row, v);
            editor.SetReason(row, reasonKey, $"test rule: {category}");
        }
        return new FfiContentPolicy(document: editor.DocumentBytes());
    }
}
