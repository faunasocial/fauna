using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;
using ContentLabelEntry = uniffi.fauna_core.ContentLabelEntry;

namespace FaunaApp.Tests;

/// <summary>
/// The content-policy render inputs + session reveal set (family-safety.md §
/// Content policy; moderation.md § Categories &amp; enforcement item 1) — the
/// feed post-card's and conversation bubble's one source of truth for "what is
/// this item's verdict".
///
/// <para>Every non-short-circuit arm calls the REAL
/// <c>FaunaFfiMethods.ContentRenderVerdict</c> export (the native dll loads in
/// the test host — reference_windows_dotnet_test_loads_native_ffi), so these
/// pin the shared compose end to end rather than a C# re-implementation. That
/// is the point: **no rule assembly lives in C#** — if one ever crept in, these
/// would keep passing while the shared engine drifted away underneath.</para>
///
/// <para>Guardian floors trigger at a hard-coded 500‰
/// (<c>GUARDIAN_FLOOR_TRIGGER_PERMILLE</c>, <c>fauna-core/src/obligation.rs</c>);
/// a viewer's own threshold triggers at the viewer's own slider value. The
/// fixtures below use 900‰ "well above" and 100‰ "well below" labels so they
/// never sit on a boundary.</para>
/// </summary>
[Collection("ActorScopedStaticsGlobal")]
public class ContentPolicyCacheTests : IDisposable
{
    // The cache is process-lifetime static state (the windows twin of linux's
    // content_policy thread-locals) — reset around each test so cases don't leak.
    //
    // Resetting is NOT sufficient on its own: xUnit parallelizes by class, so any
    // other class mutating this static must be SERIALIZED against this one —
    // hence the shared collection above (defined in ActorScopeTests.cs, which
    // explains why the three narrower collections were merged into one).
    // Without it, one class's reset lands mid-assertion in the other and the
    // failure looks like a product bug: green in isolation, red in a full run.
    public ContentPolicyCacheTests() => ContentPolicyCache.Reset();
    public void Dispose() => ContentPolicyCache.Reset();

    private static ContentLabelEntry[] Labels(params (string Category, ushort Permille)[] entries)
    {
        var result = new ContentLabelEntry[entries.Length];
        for (var i = 0; i < entries.Length; i++)
        {
            result[i] = new ContentLabelEntry(entries[i].Category, entries[i].Permille);
        }
        return result;
    }

    // ── The short-circuit ──────────────────────────────────────────────────

    /// <summary>
    /// All three inputs absent → <c>"show"</c> WITHOUT the shared-Rust call.
    ///
    /// <para>The assertion doubles as the proof that the FFI was not consulted:
    /// the label below is present and well above every trigger, so
    /// <c>ContentRenderVerdict</c> would answer <c>"badge"</c> (a present label
    /// that nothing escalated). Only the short-circuit can produce
    /// <c>"show"</c> here. <c>"badge"</c> and <c>"show"</c> are identical to the
    /// render gate, which is what makes the short-circuit safe — and it keeps a
    /// default-constructed snapshot usable in an FFI-free unit path.</para>
    /// </summary>
    [Fact]
    public void VerdictFor_AllInputsAbsent_ShortCircuitsToShowWithoutFfi()
    {
        var inputs = new ContentPolicyInputs();

        Assert.Equal("show", inputs.VerdictFor(Labels(("spam", 900))));
        Assert.Equal("show", inputs.VerdictFor(Labels()));
    }

    // ── The every-user half: the viewer's OWN thresholds ───────────────────

    /// <summary>
    /// No guardian at all, viewer's own spam threshold at 500‰, a 900‰ spam
    /// label → <c>"collapse"</c>. This is moderation.md § Categories &amp;
    /// enforcement item 1's every-user un-darking: no guardian is involved.
    /// (Both thresholds are set because they are one
    /// <c>fauna.spam.get_preferences</c> read — the shared engine composes an
    /// own-threshold rule only for a *complete* pair, see
    /// <see cref="VerdictFor_OwnThresholdsHalfKnown_ComposesNoOwnRule"/>.)
    /// </summary>
    [Fact]
    public void VerdictFor_OwnThresholdsOnly_LabelAboveThreshold_Collapses()
    {
        var inputs = new ContentPolicyInputs(null, 500, 500);

        Assert.Equal("collapse", inputs.VerdictFor(Labels(("spam", 900))));
    }

    /// <summary>
    /// Same viewer, a label BELOW their threshold → not collapsed. It still
    /// badges (a label is present, nothing escalated it), which the render gate
    /// treats exactly like <c>"show"</c>.
    /// </summary>
    [Fact]
    public void VerdictFor_OwnThresholdsOnly_LabelBelowThreshold_DoesNotCollapse()
    {
        var inputs = new ContentPolicyInputs(null, 500, 500);

        Assert.Equal("badge", inputs.VerdictFor(Labels(("spam", 100))));
    }

    /// <summary>
    /// A half-known threshold pair composes NO own-threshold rule (the shared
    /// <c>ViewerThresholds</c> is deliberately unrepresentable half-filled). The
    /// preloader therefore always sets both from the one read or neither — this
    /// pins the contract so a future caller can't half-fill it and silently lose
    /// the viewer's collapse.
    /// </summary>
    [Fact]
    public void VerdictFor_OwnThresholdsHalfKnown_ComposesNoOwnRule()
    {
        var inputs = new ContentPolicyInputs(null, 500, null);

        Assert.Equal("badge", inputs.VerdictFor(Labels(("spam", 900))));
    }

    // ── The guardian floor ─────────────────────────────────────────────────

    /// <summary>A guardian <c>block</c> floor on a category, with a label in it
    /// above the 500‰ guardian trigger → <c>"block"</c>.</summary>
    [Fact]
    public void VerdictFor_GuardianBlockFloor_Blocks()
    {
        var inputs = new ContentPolicyInputs(
            FfiContentPolicyFixture.Make(nsfw: "block"));

        Assert.Equal("block", inputs.VerdictFor(Labels(("nsfw", 900))));
    }

    /// <summary>
    /// Strictest-wins: a guardian <c>block</c> on one category beats an
    /// own-threshold <c>collapse</c> on another (family-safety.md § Content
    /// policy, `Block > Collapse > Badge > Show`).
    /// </summary>
    [Fact]
    public void VerdictFor_GuardianBlockBeatsOwnThresholdCollapse()
    {
        var inputs = new ContentPolicyInputs(
            FfiContentPolicyFixture.Make(nsfw: "block"), 500, 500);

        Assert.Equal("block", inputs.VerdictFor(Labels(("nsfw", 900), ("spam", 900))));
    }

    /// <summary>
    /// A guardian <c>inherit</c> floor adds no rule — the ward's OWN threshold
    /// decides. Below with a threshold the label clears → <c>"collapse"</c>;
    /// with a threshold it does not clear → only <c>"badge"</c>, even though the
    /// label is above the 500‰ guardian trigger (which is exactly what "inherit
    /// contributes nothing" means).
    /// </summary>
    [Fact]
    public void VerdictFor_GuardianInherit_DefersToWardOwnThreshold()
    {
        var lenientGuardian = FfiContentPolicyFixture.Make();

        var wardCollapses = new ContentPolicyInputs(lenientGuardian, 400, 400);
        Assert.Equal("collapse", wardCollapses.VerdictFor(Labels(("spam", 600))));

        var wardTolerates = new ContentPolicyInputs(lenientGuardian, 900, 900);
        Assert.Equal("badge", wardTolerates.VerdictFor(Labels(("spam", 600))));
    }

    /// <summary>
    /// Fail-closed (family-safety.md § Content policy): a floor value this
    /// client build cannot name — e.g. a newer nest's <c>quarantine</c> —
    /// renders <c>block</c>, never <c>inherit</c>. Decided in shared Rust, so
    /// windows can never drift into failing open.
    /// </summary>
    [Fact]
    public void VerdictFor_UnparseableGuardianFloor_FailsClosedToBlock()
    {
        var inputs = new ContentPolicyInputs(
            FfiContentPolicyFixture.Make(nsfw: "quarantine"));

        Assert.Equal("block", inputs.VerdictFor(Labels(("nsfw", 900))));
    }

    // ── The cache: snapshot + session reveal set ───────────────────────────

    /// <summary>The cache starts all-absent (fails closed to under-enforcement
    /// until the reads land) and delegates to whatever snapshot is set.</summary>
    [Fact]
    public void Cache_DefaultsToAbsentInputs_ThenDelegatesToTheSetSnapshot()
    {
        Assert.Equal("show", ContentPolicyCache.VerdictFor(Labels(("spam", 900))));

        ContentPolicyCache.SetOwnThresholds((500, 500));

        Assert.Equal("collapse", ContentPolicyCache.VerdictFor(Labels(("spam", 900))));
    }

    // ── Merge semantics: the two halves hydrate from two independent sites ──
    //
    // The guardian half is set by MainPage.CheckFamilyStatusAsync (UI thread,
    // riding the family-gate read); the own half by ContentPolicyPreloader (a
    // background continuation off the same Page_Loaded). Neither knows whether
    // the other has landed, so EITHER order must leave both present — a setter
    // that replaced the snapshot wholesale would silently drop whichever half
    // arrived first, and the loser is decided by RPC latency (i.e. it would be
    // flaky, not reliably broken).

    /// <summary>Own thresholds first, then the guardian floor: both survive.</summary>
    [Fact]
    public void SetGuardianPolicy_AfterOwnThresholds_DoesNotClobberThem()
    {
        ContentPolicyCache.SetOwnThresholds((500, 500));
        ContentPolicyCache.SetGuardianPolicy(
            FfiContentPolicyFixture.Make(nsfw: "block"));

        // The guardian half is live...
        Assert.Equal("block", ContentPolicyCache.VerdictFor(Labels(("nsfw", 900))));
        // ...and the own half was not clobbered by the later setter.
        Assert.Equal("collapse", ContentPolicyCache.VerdictFor(Labels(("spam", 900))));
    }

    /// <summary>Guardian floor first, then own thresholds: both survive.</summary>
    [Fact]
    public void SetOwnThresholds_AfterGuardianPolicy_DoesNotClobberIt()
    {
        ContentPolicyCache.SetGuardianPolicy(
            FfiContentPolicyFixture.Make(nsfw: "block"));
        ContentPolicyCache.SetOwnThresholds((500, 500));

        // The own half is live...
        Assert.Equal("collapse", ContentPolicyCache.VerdictFor(Labels(("spam", 900))));
        // ...and the guardian half was not clobbered by the later setter.
        Assert.Equal("block", ContentPolicyCache.VerdictFor(Labels(("nsfw", 900))));
    }

    /// <summary>
    /// A null guardian policy is a REAL value ("unsupervised — no floor"), not a
    /// skip: it clears a previously-set floor (the re-login-as-another-account
    /// case) while leaving the viewer's own thresholds intact.
    /// </summary>
    [Fact]
    public void SetGuardianPolicy_Null_ClearsOnlyTheGuardianHalf()
    {
        ContentPolicyCache.SetOwnThresholds((500, 500));
        ContentPolicyCache.SetGuardianPolicy(
            FfiContentPolicyFixture.Make(nsfw: "block"));

        ContentPolicyCache.SetGuardianPolicy(null);

        Assert.Equal("badge", ContentPolicyCache.VerdictFor(Labels(("nsfw", 900))));
        Assert.Equal("collapse", ContentPolicyCache.VerdictFor(Labels(("spam", 900))));
    }

    /// <summary>Symmetrically, clearing the own thresholds leaves the guardian
    /// floor enforcing.</summary>
    [Fact]
    public void SetOwnThresholds_Null_ClearsOnlyTheOwnHalf()
    {
        ContentPolicyCache.SetOwnThresholds((500, 500));
        ContentPolicyCache.SetGuardianPolicy(
            FfiContentPolicyFixture.Make(nsfw: "block"));

        ContentPolicyCache.SetOwnThresholds(null);

        Assert.Equal("badge", ContentPolicyCache.VerdictFor(Labels(("spam", 900))));
        Assert.Equal("block", ContentPolicyCache.VerdictFor(Labels(("nsfw", 900))));
    }

    /// <summary>The session-local reveal set: nothing revealed by default, one
    /// item at a time, and <c>Reset</c> clears it.</summary>
    [Fact]
    public void Reveal_IsSessionLocalPerItemAndClearedByReset()
    {
        Assert.False(ContentPolicyCache.IsRevealed("post-1"));

        ContentPolicyCache.Reveal("post-1");

        Assert.True(ContentPolicyCache.IsRevealed("post-1"));
        Assert.False(ContentPolicyCache.IsRevealed("post-2"));

        ContentPolicyCache.Reset();

        Assert.False(ContentPolicyCache.IsRevealed("post-1"));
    }

    /// <summary><c>Reset</c> clears the snapshot too, not only the
    /// reveal set — otherwise a populated cache would leak into the next test
    /// and look like a product bug.</summary>
    [Fact]
    public void Reset_ClearsTheSnapshot()
    {
        ContentPolicyCache.SetOwnThresholds((500, 500));
        ContentPolicyCache.SetGuardianPolicy(
            FfiContentPolicyFixture.Make(nsfw: "block"));

        ContentPolicyCache.Reset();

        Assert.Equal("show", ContentPolicyCache.VerdictFor(Labels(("nsfw", 900))));

        Assert.Equal("show", ContentPolicyCache.VerdictFor(Labels(("spam", 900))));
    }
}
