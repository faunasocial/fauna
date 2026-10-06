using System;
using System.Collections.Generic;
using System.Linq;
using uniffi.fauna_core;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Session-local muted-keyword list cache + per-message reveal set
/// (moderation.md § Muted keywords; content-moderation-and-ranking.md § Q3).
/// Native twin of linux's <c>crate::conversations</c> thread-locals
/// (<c>MUTED_KEYWORDS</c> / <c>REVEALED_MUTED</c>) — a process-lifetime cache
/// so the conversation bubble collapse (<c>ConversationsPage.ToMessageView</c>
/// → <c>DmMessageBubble.Bind</c>) doesn't need the list threaded through every
/// page/VM. Populated once at login (<c>App.StartMainAppAsync</c>) and
/// refreshed on every Settings "Muted words" page load/save
/// (<c>MutedWordsViewModel</c>) — both run before any conversation bubble
/// renders for this session. Revealing a muted message ("show anyway")
/// un-collapses it for the rest of the session; the mute itself is
/// unaffected — un-muting the term is the only way to stop future messages
/// collapsing.
/// </summary>
public static class MutedKeywordsCache
{
    private static MutedKeyword[] _keywords = Array.Empty<MutedKeyword>();
    private static readonly HashSet<string> _revealed = new();

    /// The current muted-keyword list — each entry's term and weight.
    ///
    /// <remarks><c>internal</c>, like <see cref="SetKeywords"/>, and forced rather
    /// than chosen: <c>MutedKeyword</c> is a UniFFI-generated type, which
    /// <c>uniffi-bindgen-cs</c> always emits as <c>internal</c>, so a <c>public</c>
    /// member of this <c>public</c> class naming it is CS0053/CS0051 and the whole
    /// assembly fails to build (<see cref="AccountStateDir.EraseAll"/>'s remarks
    /// own the rule). Both callers are inside <c>[InternalsVisibleTo]</c>.</remarks>
    internal static IReadOnlyList<MutedKeyword> Keywords => _keywords;

    /// The cached terms, in stored order.
    public static IReadOnlyList<string> Words => _keywords.Select(k => k.keyword).ToArray();

    /// Replace the cached list — called after every load/save of the Settings
    /// "Muted words" page and once at login.
    internal static void SetKeywords(IReadOnlyList<MutedKeyword> keywords) =>
        _keywords = keywords as MutedKeyword[] ?? keywords.ToArray();

    /// Whether <paramref name="messageId"/> has been revealed ("show anyway")
    /// this session.
    public static bool IsRevealed(string messageId) => _revealed.Contains(messageId);

    /// Mark <paramref name="messageId"/> as revealed for the rest of the session.
    public static void Reveal(string messageId) => _revealed.Add(messageId);

    /// Does <paramref name="body"/> collapse behind the cached list, and isn't
    /// already revealed for <paramref name="messageId"/>? The one call
    /// <c>ConversationsPage.ToMessageView</c> needs to compute
    /// <c>DmMessageView.Muted</c> — combines the shared collapse decision (only
    /// a term muted at the full penalty collapses) with the session-local
    /// reveal so the bubble collapse and the cache never fork the "is this
    /// message muted" answer.
    public static bool IsMuted(string body, string messageId) =>
        !IsRevealed(messageId) && FaunaFfiMethods.MatchesMutedKeywords(body, _keywords);

    /// Drop the cached word list and this session's reveal set.
    ///
    /// <para>Called in production by <see cref="ActorScope.DropActorScopedState"/>
    /// on every actor change — process-lifetime static state that no shell
    /// teardown drops for us. Tests that populate it clear it first too.</para>
    internal static void Reset()
    {
        _keywords = Array.Empty<MutedKeyword>();
        _revealed.Clear();
    }
}
