namespace FaunaApp.Core.Helpers;

/// <summary>
/// Composition for the test agent's deferred "post actions" — the UI-thread work a
/// <c>set_state</c> command defers until after its state has been pushed (navigation,
/// dialog dismissal, InfoBar updates).
///
/// <para>A single <c>set_state</c> may carry several blocks that each need UI-thread
/// work (<c>session</c> + <c>nav</c> + <c>messages</c> + <c>compose</c> all ride one
/// command). Each block <b>composes</b> its work onto whatever earlier blocks queued,
/// via <see cref="Then"/> — no block may overwrite an earlier block's action.</para>
///
/// <para><b>Why this is a <c>Func&lt;Task&gt;</c> and not an <c>Action</c>.</b> These
/// actions genuinely await (a conversations session, a page's async load). Composing
/// them as <c>Action</c>s made every lambda <c>async void</c>: invoking one returned at
/// its first <c>await</c>, so a "chained" follow-up ran <b>before</b> the action it was
/// sequenced after had finished — reading state the previous step had not yet written.
/// That is not a theoretical hazard: the <c>nav</c> block sidestepped it by overwriting
/// the <c>session</c> block's action outright, which left <c>MainPage</c> bound to the
/// clients <c>DisposeNestClients</c> had just disposed on every second-or-later login.
/// Awaiting the chain is what makes ordering real.</para>
/// </summary>
internal static class PostActionChain
{
    /// <summary>
    /// Sequence <paramref name="next"/> after <paramref name="first"/>, awaiting
    /// <paramref name="first"/> to completion before <paramref name="next"/> starts.
    /// A null <paramref name="first"/> (nothing queued yet) yields
    /// <paramref name="next"/> alone.
    /// </summary>
    internal static Func<Task> Then(this Func<Task>? first, Func<Task> next)
    {
        ArgumentNullException.ThrowIfNull(next);
        if (first is null) return next;
        return async () =>
        {
            await first();
            await next();
        };
    }
}
