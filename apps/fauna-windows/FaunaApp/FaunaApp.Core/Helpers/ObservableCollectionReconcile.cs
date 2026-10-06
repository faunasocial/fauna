using System;
using System.Collections.Generic;
using System.Collections.ObjectModel;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// In-place, keyed reconcile of a bound <see cref="ObservableCollection{T}"/> against a
/// freshly-projected <paramref name="target"/> list. An <b>unchanged</b> row keeps its
/// existing instance — so WinUI does not regenerate its container and any in-flight async
/// load (e.g. a post-card's <c>/api/v1/blob/&lt;hash&gt;</c> image fetch) survives — while a
/// changed row is replaced, a new row inserted, a removed row dropped, and a reordered row
/// moved. Only the rows that actually differ raise a <c>CollectionChanged</c> event.
///
/// <para>This replaces the <c>Clear()</c>+rebuild-all-rows pattern, whose per-tick teardown
/// of every container restarted (and so never settled) a stable row's in-flight image load —
/// the feed image-render gap (render-model.md § D6; feed.md § State &amp; data shape). Kept
/// generic so the feed post list, and later the conversations thread/message lists, can share
/// one shape rather than each re-deriving the diff (priority #2/#4 — the richest existing
/// pattern, generalizing FeedPage's coarse <c>SyncFeeds</c>/<c>SyncAvailableBridges</c> id-set
/// guards to per-row content).</para>
/// </summary>
public static class ObservableCollectionReconcile
{
    /// <param name="live">The bound collection to mutate in place toward <paramref name="target"/>.</param>
    /// <param name="target">The desired ordered rows. Keys MUST be unique within it (the feed
    /// snapshot is deduplicated by post id).</param>
    /// <param name="keyOf">Stable identity of a row (e.g. the post id) — decides reuse vs insert/remove.</param>
    /// <param name="contentEquals">True iff two same-key rows render identically, so the existing
    /// <paramref name="live"/> instance (and its realized container + in-flight loads) is kept.</param>
    public static void Reconcile<T>(
        ObservableCollection<T> live,
        IReadOnlyList<T> target,
        Func<T, string> keyOf,
        Func<T, T, bool> contentEquals)
    {
        // 1. Drop live rows whose key is absent from the target.
        var targetKeys = new HashSet<string>(target.Count);
        foreach (var t in target)
            targetKeys.Add(keyOf(t));
        for (int i = live.Count - 1; i >= 0; i--)
            if (!targetKeys.Contains(keyOf(live[i])))
                live.RemoveAt(i);

        // 2. Align live[i] to target[i] by key, position by position. Reuse the existing
        //    instance (keeping its container + in-flight loads) when its content is unchanged;
        //    replace only when the content actually differs.
        for (int i = 0; i < target.Count; i++)
        {
            var want = target[i];
            var wantKey = keyOf(want);

            if (i < live.Count && keyOf(live[i]) == wantKey)
            {
                if (!contentEquals(live[i], want))
                    live[i] = want;
                continue;
            }

            // The wanted key isn't at position i: find it further down (a reorder/removal
            // shifted it) and move it into place; otherwise it's a brand-new row.
            int found = -1;
            for (int j = i + 1; j < live.Count; j++)
                if (keyOf(live[j]) == wantKey)
                {
                    found = j;
                    break;
                }

            if (found >= 0)
            {
                live.Move(found, i);
                if (!contentEquals(live[i], want))
                    live[i] = want;
            }
            else
            {
                live.Insert(i, want);
            }
        }

        // 3. Trim any trailing rows beyond the target length. With unique keys steps 1–2 are
        //    already exact; this is a defensive guard against accidental duplicate keys.
        while (live.Count > target.Count)
            live.RemoveAt(live.Count - 1);
    }
}
