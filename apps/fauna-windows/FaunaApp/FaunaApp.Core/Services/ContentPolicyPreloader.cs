using System;
using System.Threading.Tasks;
using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Services;

/// <summary>
/// Hydrates the <b>own-thresholds half</b> of <see cref="ContentPolicyCache"/> —
/// the viewer's <c>fauna.spam.get_preferences</c> spam/phishing per-mille
/// sliders (moderation.md § Categories &amp; enforcement item 1's every-user
/// un-darking; family-safety.md § Content policy). The native twin of linux's
/// post-auth <c>content_policy::set_spam_preferences</c> seed.
///
/// <para><b>The guardian half is NOT read here.</b> It rides the
/// <c>fauna.family.status</c> read <c>MainPage.CheckFamilyStatusAsync</c>
/// already makes for the <c>family-tab</c>/<c>supervised-indicator</c> gate,
/// which hands the policy straight to
/// <see cref="ContentPolicyCache.SetGuardianPolicy"/> — same handler, no second
/// RPC, and the gate chrome it reveals is the e2e-visible witness that the floor
/// is live. The two halves stay independent by construction: this read failing
/// cannot cost a supervised viewer their guardian floor, and that read failing
/// cannot cost anyone their own thresholds (which is the ONLY half an
/// unsupervised viewer has).</para>
///
/// <para>Fired fire-and-forget from <c>MainPage.Page_Loaded</c> — the one site on
/// EVERY login path. The production <c>App.StartMainAppAsync</c> path is
/// <b>not</b>: the e2e <c>set_state</c> login never enters it, so hydrating
/// there would leave the cache empty under test and every content-policy render
/// assertion would fail with the product code perfectly correct — the exact
/// harness-gap-presenting-as-a-product-bug that family-safety.md § Content
/// policy records against the apple leg.</para>
///
/// <para>Best-effort: a fault must never disrupt login (the HostAddressReporter /
/// MutedKeywordsPreloader stance). On failure the half simply stays absent,
/// which can only <i>under</i>-enforce a collapse, never wrongly block.</para>
/// </summary>
internal sealed class ContentPolicyPreloader
{
    private readonly INestRpcClient _rpc;

    public ContentPolicyPreloader(INestRpcClient rpc) => _rpc = rpc;

    public async Task RunAsync()
    {
        await RefreshOwnThresholdsAsync().ConfigureAwait(false);

        // The third input: what the owner's own reports hid (moderation.md §
        // Corollary — block also hides; `load_hidden_content`). Independent of the
        // two reads above — a failure here leaves only this half absent, which
        // under-enforces (a reported item paints again), never wrongly hides.
        // Fired from the same fire-and-forget site, so no surface awaits it on its
        // mount path (web's first run hung the feed behind exactly that await).
        await RefreshHiddenContentAsync().ConfigureAwait(false);
    }

    /// <summary>
    /// (Re-)read the viewer's own spam/phishing thresholds into
    /// <see cref="ContentPolicyCache"/> — fired at cold launch (<see cref="RunAsync"/>)
    /// AND on every WS reconnect (<c>MainPage.OnRpcReconnected</c>;
    /// family-client-enforcement.md § Content policy, the own-threshold bullet), so
    /// a login read that lost the socket cannot leave the own-threshold collapse
    /// off for the session. A failed read keeps the loaded thresholds (nothing is
    /// cleared); a reply that lands after the signed-in identity changed is
    /// dropped, never written over the next identity's cache — the
    /// <see cref="ActorScope"/> generation token taken before the read is the
    /// identity stamp. Never throws.
    /// </summary>
    internal async Task RefreshOwnThresholdsAsync()
    {
        using var scope = ActorScope.BeginBackgroundLoop();
        try
        {
            // ConfigureAwait(false) is legal here and deliberate: this is a
            // service with no UI-thread continuation after the await (the cache
            // setter is thread-safe). It must NOT appear anywhere in
            // CheckFamilyStatusAsync's chain, which touches NavFamily /
            // SupervisedIndicatorButton after its await and would throw
            // COMException off the UI thread.
            var prefs = await _rpc.SpamGetPreferencesAsync().ConfigureAwait(false);
            if (scope.Token.IsCancellationRequested)
            {
                ShellLog.Warn(
                    "ContentPolicyPreloader",
                    "own thresholds reply dropped: signed-in identity changed during the read");
                return;
            }
            // Both thresholds or neither — one read, one tuple (see
            // ContentPolicyCache.SetOwnThresholds for why the pair is a type).
            ContentPolicyCache.SetOwnThresholds((prefs.spamThreshold, prefs.phishingThreshold));
        }
        catch (Exception ex)
        {
            ShellLog.Warn(
                "ContentPolicyPreloader",
                $"own thresholds skipped: {ex.GetType().Name}: {ex.Message}");
        }
    }

    /// <summary>
    /// Re-read the owner's hide list into <see cref="ContentPolicyCache"/> — the
    /// store-change notice's app-wide arm (<c>account-runtime.md</c> § A runtime's
    /// own pump is a source of the notice too, part 4): the list is a render
    /// source for every surface, not one page's, so it re-reads on every notice
    /// whatever page is open. A failure keeps the cached list (under-enforces, never
    /// wrongly hides); an unchanged answer rewrites nothing. Never throws.
    /// </summary>
    internal async Task RefreshHiddenContentAsync()
    {
        try
        {
            var hidden = await _rpc.LoadHiddenContentAsync().ConfigureAwait(false);
            if (!ContentPolicyCache.HiddenContentEquals(hidden))
                ContentPolicyCache.SetHiddenContent(hidden);
        }
        catch (Exception ex)
        {
            ShellLog.Warn(
                "ContentPolicyPreloader",
                $"hidden content skipped: {ex.GetType().Name}: {ex.Message}");
        }
    }
}
