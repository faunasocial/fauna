using System;
using System.Linq;
using System.Text;
using System.Threading.Tasks;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_client_policy;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The shared <c>rule-editor</c>'s windows state (<see cref="RuleEditorModel"/>, the
/// twin of tui's <c>rule_editor::Mount</c>) over the REAL shared editor
/// (<c>FfiRuleEditor</c>, the native <c>fauna_ffi</c> dll loads in the test host) —
/// <c>nest-rules.md</c> § The shared rule editor. Pins the paint the e2e contract
/// reads (one subject control by kind, no level on a keywords rule, the save gate,
/// the status line) and the admin mounting's write (<see cref="RuleEditorHosts"/>).
/// Labels are compared through <see cref="Strings"/> at assertion time, so the
/// tests hold whichever localizer the process carries.
/// </summary>
[Collection("StringsGlobal")]
public class RuleEditorModelTests
{
    private static RuleEditorModel NestModel()
    {
        var model = new RuleEditorModel();
        model.Load(FfiRuleEditor.ForNest("example.test", null));
        return model;
    }

    private static int KindIndex(RuleEditorModel model, RuleKind kind) =>
        Array.FindIndex(model.Editor!.View().kinds, k => k.kind == kind);

    [Fact]
    public void AnUnloadedModel_PaintsOnlyItsLoadingStatus()
    {
        var paint = new RuleEditorModel().Paint();

        Assert.False(paint.Loaded);
        Assert.Empty(paint.Rows);
        Assert.False(paint.SaveEnabled);
        Assert.Equal(Strings.Get("rule_editor/loading"), paint.Status);
    }

    [Fact]
    public void ANewRule_IsACategoryRuleWithALevel_ThatCannotBeSavedWithoutAReason()
    {
        var model = NestModel();
        Assert.Empty(model.Paint().Rows);

        Assert.True(model.AddRule());
        var paint = model.Paint();

        var row = Assert.Single(paint.Rows);
        Assert.Equal(RuleSubject.Category, row.Subject.Kind);
        Assert.NotNull(row.LevelOptions);
        Assert.True(row.Live);
        Assert.False(paint.SaveEnabled);
        // The crate's defect sentence is the status before any host verdict.
        Assert.False(string.IsNullOrEmpty(paint.Status));
        // Every picker option is the label the crate painted.
        Assert.Equal(
            model.Editor!.View().kinds.Select(k => Strings.Resolve(k.label)).ToArray(),
            row.KindOptions.ToArray());
    }

    [Fact]
    public void SwitchingTheKind_SwapsTheSubjectControl_AndAKeywordsRuleHasNoLevel()
    {
        var model = NestModel();
        model.AddRule();

        Assert.True(model.PickKind(0, KindIndex(model, RuleKind.Keywords)));
        var row = Assert.Single(model.Paint().Rows);

        Assert.Equal(RuleSubject.Keywords, row.Subject.Kind);
        Assert.Null(row.LevelOptions);
        Assert.Equal(Strings.Resolve(model.Editor!.View().kinds[KindIndex(model, RuleKind.Keywords)].label), row.KindLabel);
    }

    [Fact]
    public void TypedKeywordsAndAReason_MakeTheDocumentSaveable()
    {
        var model = NestModel();
        model.AddRule();
        model.PickKind(0, KindIndex(model, RuleKind.Keywords));

        // A trailing comma mid-typing survives the repaint.
        Assert.True(model.SetKeywords(0, "casino,"));
        Assert.Equal("casino,", model.Paint().Rows[0].Subject.Text);
        Assert.True(model.SetKeywords(0, "casino, lottery"));
        Assert.True(model.SetReason(0, "No gambling here"));

        var paint = model.Paint();
        Assert.True(paint.SaveEnabled);
        Assert.Equal("", paint.Status);
        Assert.Equal("No gambling here", paint.Rows[0].Reason);
        Assert.NotEmpty(model.Editor!.DocumentBytes());
    }

    /// <summary>A picker re-raising the value it was painted with, or a
    /// <c>TextChanged</c> echoing the painted text, is no edit — so the "Saved" a
    /// host just set survives the repaint that follows it; a real edit drops it.</summary>
    [Fact]
    public void AnEchoOfThePaintedValueIsNoEdit_AndKeepsTheHostsStatus()
    {
        var model = NestModel();
        model.AddRule();
        model.SetReason(0, "A reason");
        model.Status = "Saved";
        var row = model.Paint().Rows[0];

        Assert.False(model.SetReason(0, "A reason"));
        Assert.False(model.PickVerdict(0, row.VerdictSelected));
        Assert.False(model.PickKind(0, row.KindSelected));
        Assert.Equal("Saved", model.Paint().Status);

        Assert.True(model.SetReason(0, "Another reason"));
        Assert.Null(model.Status);
    }

    [Fact]
    public void Remove_DropsTheRule()
    {
        var model = NestModel();
        model.AddRule();

        Assert.True(model.RemoveRule(0));

        Assert.Empty(model.Paint().Rows);
        Assert.False(model.RemoveRule(0));
    }

    /// <summary>The labeler picker lists the publisher's subscriptions; picking one the
    /// document does not bundle fetches its module (by the id's bytes) and bundles it
    /// under the choice's name.</summary>
    [Fact]
    public async Task PickingASubscribedLabeler_FetchesItsModuleAndBundlesIt()
    {
        var id = string.Concat(Enumerable.Repeat("ab", 32));
        var model = NestModel();
        model.Subscribed = new[] { id };
        model.AddRule();
        model.PickKind(0, KindIndex(model, RuleKind.Labeler));
        var subject = model.Paint().Rows[0].Subject;
        Assert.Equal(RuleSubject.Labeler, subject.Kind);
        var name = Assert.Single(subject.Options);

        byte[]? asked = null;
        var ok = await model.PickLabelerAsync(0, 0, labelerId =>
        {
            asked = labelerId;
            return Task.FromResult<byte[]?>(Encoding.UTF8.GetBytes("module"));
        });

        Assert.True(ok);
        Assert.Equal(Convert.FromHexString(id), asked);
        Assert.Equal(name, model.Editor!.View().rows[0].labeler);
        Assert.Equal(0, model.Paint().Rows[0].Subject.Selected);
    }

    // ── The admin mounting's write ──────────────────────────────────────

    [Fact]
    public async Task AnAdminSave_NamesTheVersionAndReseedsTheEditorFromTheStoredDocument()
    {
        var model = NestModel();
        model.AddRule();
        model.SetReason(0, "A reason");
        var rpc = new MockNestRpcClient();

        var outcome = await RuleEditorHosts.SaveNestAsync(rpc, "example.test", model.Editor!.DocumentBytes());

        Assert.Equal(RuleEditorHosts.SavedVersion(1), outcome.Status);
        var reload = Assert.IsType<FfiRuleEditor>(outcome.Reload);
        Assert.Single(reload.View().rows);
        Assert.Single(rpc.AdminContentPolicyWrites);
    }

    [Fact]
    public async Task AnAdminSaveTheNestRefuses_NamesItsDefectAndKeepsTheEditor()
    {
        var model = NestModel();
        model.AddRule();
        model.SetReason(0, "A reason");
        var rpc = new MockNestRpcClient
        {
            NextAdminSetContentPolicy = new FfiAdminContentPolicy(null, 0, 0, "the nest's own sentence"),
        };

        var outcome = await RuleEditorHosts.SaveNestAsync(rpc, "example.test", model.Editor!.DocumentBytes());

        Assert.Equal("the nest's own sentence", outcome.Status);
        Assert.Null(outcome.Reload);
    }
}
