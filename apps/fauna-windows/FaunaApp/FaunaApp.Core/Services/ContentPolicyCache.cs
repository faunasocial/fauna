using System.Collections.Generic;
using uniffi.fauna_ffi;
using ContentLabelEntry = uniffi.fauna_core.ContentLabelEntry;

namespace FaunaApp.Core.Services;

/// <summary>
/// The immutable snapshot of the two inputs the shared content-policy render
/// engine needs (family-safety.md § Content policy) — the windows twin of
/// linux's <c>crate::content_policy</c> thread-locals, web's
/// <c>contentPolicy.svelte.ts</c> and android's <c>ContentPolicyInputs</c>
/// (priority #2/#3, one concept everywhere):
///
/// <list type="number">
/// <item>the guardian's per-category floor (<c>fauna.family.status</c>'s
/// <c>policy.content_policy</c>; <c>null</c> unless supervised), and</item>
/// <item>the viewer's OWN spam/phishing per-mille thresholds
/// (<c>fauna.spam.get_preferences</c>) — the every-user un-darking of
/// moderation.md § Categories &amp; enforcement item 1.</item>
/// </list>
///
/// A record (not the cache) so a render surface can memoize a verdict on it and
/// a unit test can drive <see cref="VerdictFor"/> with fixed inputs, no cache
/// mutation needed. Both social surfaces (feed post-card, conversation bubble)
/// resolve an item through <see cref="VerdictFor"/>, which composes the two
/// **strictest-wins, entirely in shared Rust** — no rule assembly lives in C#,
/// exactly as the linux/web/android legs keep it out of their shells.
/// </summary>
internal sealed record ContentPolicyInputs(
    FfiContentPolicy? ContentPolicy = null,
    ushort? OwnSpamPermille = null,
    ushort? OwnPhishingPermille = null)
{
    /// <summary>
    /// The client render verdict for one item's <paramref name="labels"/> — one
    /// of <c>"show" | "badge" | "collapse" | "block"</c>, resolved by the shared
    /// <c>FaunaFfiMethods.ContentRenderVerdict</c> (strictest-wins compose of the
    /// guardian floor and the viewer's own thresholds, all in Rust).
    ///
    /// <para>When there is neither a guardian floor nor an own threshold, no rule
    /// can fire, so the verdict is at most <c>"badge"</c> — and
    /// <c>"badge"</c>/<c>"show"</c> are identical to the render gate (both render
    /// normally). This short-circuits to <c>"show"</c> without the shared-Rust
    /// call, which keeps a default-constructed <see cref="ContentPolicyInputs"/>
    /// FFI-free (the same short-circuit android's
    /// <c>ContentPolicyInputs.verdictFor</c> makes).</para>
    /// </summary>
    public string VerdictFor(ContentLabelEntry[] labels) =>
        ContentPolicy is null && OwnSpamPermille is null && OwnPhishingPermille is null
            ? "show"
            : FaunaFfiMethods.ContentRenderVerdict(
                labels, ContentPolicy, OwnSpamPermille, OwnPhishingPermille);
}

/// <summary>
/// Process-lifetime holder for the current <see cref="ContentPolicyInputs"/> +
/// the session-local reveal set (family-safety.md § Content policy). Direct
/// sibling of <see cref="MutedKeywordsCache"/> — a static cache so the feed
/// post-card and the conversation bubble share ONE answer to "what is this
/// item's verdict" without the two inputs being threaded through every page/VM
/// (and so the two surfaces can never drift on how a floor is enforced).
///
/// <para>The two halves are hydrated by two INDEPENDENT sites, both on the
/// <c>MainPage.Page_Loaded</c> path so both run on EVERY login path, e2e
/// included (family-safety.md § Content policy):</para>
/// <list type="bullet">
/// <item>the guardian floor rides the <c>fauna.family.status</c> read
/// <c>MainPage.CheckFamilyStatusAsync</c> already makes for the
/// <c>family-tab</c>/<c>supervised-indicator</c> gate — no extra RPC, and the
/// gate chrome is the e2e-visible witness that the floor is live;</item>
/// <item>the own thresholds come from <see cref="ContentPolicyPreloader"/>'s
/// <c>fauna.spam.get_preferences</c> read, fired from the same handler.</item>
/// </list>
///
/// <para>Independent is load-bearing twice over: a failure of either read leaves
/// only its own half absent, and an UNSUPERVISED viewer (no guardian floor at
/// all) still gets the every-user own-threshold collapse. Until a read lands —
/// and after any failure — that half stays absent, which fails **closed to
/// under-enforcement**: a labeled item then at most badges, never wrongly
/// blocks.</para>
///
/// <para>Revealing a collapsed item ("show anyway") un-collapses it for the rest
/// of the session, exactly like the muted-keyword reveal. A <c>"block"</c> is
/// NOT revealable — the render surfaces check block ahead of the reveal arm
/// (family-safety.md § Content policy, the linux reference leg).</para>
///
/// <para>The app-assembly surface is deliberately plain-typed
/// (<see cref="IsRevealed"/>/<see cref="Reveal"/> take a string, the verdict
/// comes back a string): <c>FfiContentPolicy</c> and <c>ContentLabelEntry</c>
/// are UniFFI-<c>internal</c> types, so they never escape into XAML binding
/// surfaces — the same boundary <see cref="MutedKeywordsCache.IsMuted"/> keeps.</para>
/// </summary>
public static class ContentPolicyCache
{
    /// <summary>
    /// The current snapshot. <c>volatile</c> + whole-reference swap: the record
    /// is immutable, so a reader always sees a COMPLETE snapshot with no lock —
    /// never a half-applied merge. <see cref="_gate"/> serializes only the
    /// read-modify-write inside the two setters, which genuinely race: the
    /// guardian half is set on the UI thread (<c>CheckFamilyStatusAsync</c>) and
    /// the own-thresholds half off a background continuation
    /// (<see cref="ContentPolicyPreloader"/>), both from the one
    /// <c>Page_Loaded</c> handler.
    /// </summary>
    private static volatile ContentPolicyInputs _inputs = new();

    /// Serializes the two setters' merge (see <see cref="_inputs"/>). Not held
    /// across any RPC — only across the record swap.
    private static readonly object _gate = new();

    // The reveal set is deliberately NOT under _gate: unlike the snapshot it is
    // touched only from the UI thread (a reveal is a button tap; the render that
    // reads it is the same thread), exactly like MutedKeywordsCache's.
    private static readonly HashSet<string> _revealed = new();

    /// The current render-engine inputs; each half absent until its read lands.
    internal static ContentPolicyInputs Current => _inputs;

    /// <summary>
    /// Set the guardian's per-category floor half — <c>null</c> when the viewer
    /// is unsupervised, which is a REAL value ("no floor"), not a skip. Merges:
    /// leaves the own-thresholds half exactly as it was.
    /// </summary>
    internal static void SetGuardianPolicy(FfiContentPolicy? policy)
    {
        lock (_gate)
        {
            _inputs = _inputs with { ContentPolicy = policy };
        }
    }

    /// <summary>
    /// Set the viewer's own spam/phishing thresholds half — <c>null</c> when the
    /// read has not landed (or failed). Merges: leaves the guardian half exactly
    /// as it was.
    ///
    /// <para>The pair is ONE nullable tuple, not two nullable arguments, so a
    /// half-known pair is unrepresentable at this seam — the same shape, and the
    /// same reason, as shared Rust's <c>ViewerThresholds</c>: the two are one
    /// <c>fauna.spam.get_preferences</c> read, and a half-filled pair composes NO
    /// own-threshold rule at all, i.e. it fails OPEN. Encoding "both or neither"
    /// in the type is what stops a future caller reintroducing that silently.</para>
    /// </summary>
    internal static void SetOwnThresholds((ushort Spam, ushort Phishing)? thresholds)
    {
        lock (_gate)
        {
            _inputs = _inputs with
            {
                OwnSpamPermille = thresholds?.Spam,
                OwnPhishingPermille = thresholds?.Phishing,
            };
        }
    }

    /// <summary>The verdict for one item's labels under the current snapshot —
    /// the one call both render surfaces make. See
    /// <see cref="ContentPolicyInputs.VerdictFor"/>.</summary>
    internal static string VerdictFor(ContentLabelEntry[] labels) => _inputs.VerdictFor(labels);

    /// <summary>The render decision for one item under the current snapshot with the
    /// <b>region content policy</b> composed in as the third strictest-wins source
    /// (region-blocking.md § Where it composes) — the one call every social render
    /// surface (feed card, post detail, conversation bubble) makes. Its
    /// <c>Placeholder</c> is the region arm, painted ahead of the family arm. With no
    /// region plane open it is exactly <see cref="VerdictFor"/>.</summary>
    internal static RegionRenderDecision RenderFor(ContentLabelEntry[] labels, RegionSubject subject) =>
        RegionPlaneHost.Render(labels, _inputs, subject);

    /// Whether <paramref name="itemId"/> has been revealed ("show anyway") this
    /// session. Only ever consulted for a <c>"collapse"</c> verdict.
    public static bool IsRevealed(string itemId) => _revealed.Contains(itemId);

    /// Mark <paramref name="itemId"/> as revealed for the rest of the session.
    public static void Reveal(string itemId) => _revealed.Add(itemId);

    /// Drop everything this cache holds for the signed-in account: the guardian
    /// floor, the viewer's own thresholds, and this session's reveal set.
    ///
    /// <para>Called in production by <see cref="ActorScope.DropActorScopedState"/>
    /// on every actor change — the cache is process-lifetime static state and the
    /// app never exits on a switch, sign-out or factory-reset, so nothing else
    /// drops it. Tests that populate it also clear it first (and must share a
    /// serializing xUnit collection, since the runner parallelizes by class).</para>
    internal static void Reset()
    {
        lock (_gate)
        {
            _inputs = new ContentPolicyInputs();
        }
        _revealed.Clear();
    }
}
