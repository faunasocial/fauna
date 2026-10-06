using System.Collections.ObjectModel;
using System.Linq;
using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The generic in-place reconcile that backs <c>FeedPage.SyncPosts</c>'s diff-guard
/// (the feed image-render fix): an UNCHANGED row keeps its existing instance — and
/// therefore its already-realized WinUI container plus any in-flight async image-blob
/// load — while only a changed/new/removed/reordered row mutates the bound collection.
/// The pre-fix <c>Posts.Clear()</c>+rebuild-all on every observer tick tore down a
/// stable post's in-flight <c>/api/v1/blob/&lt;hash&gt;</c> fetch before it settled
/// (render-model.md § D6; feed.md § State &amp; data shape).
///
/// Reference identity of a kept row is the testable proxy for "WinUI does not
/// regenerate the container" — the property the fix relies on.
/// </summary>
public class ObservableCollectionReconcileTests
{
    private sealed class Row
    {
        public string Key { get; init; } = "";
        public int Content { get; init; }
    }

    private static void Reconcile(ObservableCollection<Row> live, params Row[] target) =>
        ObservableCollectionReconcile.Reconcile(
            live, target, r => r.Key, (a, b) => a.Content == b.Content);

    [Fact]
    public void UnchangedRow_KeepsSameInstance()
    {
        var a1 = new Row { Key = "a", Content = 1 };
        var live = new ObservableCollection<Row> { a1 };

        // A content-equal target for the same key must NOT replace the live instance.
        Reconcile(live, new Row { Key = "a", Content = 1 });

        Assert.Single(live);
        Assert.Same(a1, live[0]);
    }

    [Fact]
    public void ChangedRow_ReplacedWithTargetInstance()
    {
        var a1 = new Row { Key = "a", Content = 1 };
        var a2 = new Row { Key = "a", Content = 2 };
        var live = new ObservableCollection<Row> { a1 };

        Reconcile(live, a2);

        Assert.Single(live);
        Assert.Same(a2, live[0]);
    }

    [Fact]
    public void NewRow_InsertedAndExistingPreserved()
    {
        var a = new Row { Key = "a", Content = 1 };
        var live = new ObservableCollection<Row> { a };
        var b = new Row { Key = "b", Content = 9 };

        Reconcile(live, new Row { Key = "a", Content = 1 }, b);

        Assert.Equal(2, live.Count);
        Assert.Same(a, live[0]);
        Assert.Same(b, live[1]);
    }

    [Fact]
    public void RemovedRow_Dropped()
    {
        var a = new Row { Key = "a", Content = 1 };
        var b = new Row { Key = "b", Content = 2 };
        var live = new ObservableCollection<Row> { a, b };

        Reconcile(live, new Row { Key = "a", Content = 1 });

        Assert.Single(live);
        Assert.Same(a, live[0]);
    }

    [Fact]
    public void Reordered_PreservesInstances()
    {
        var a = new Row { Key = "a", Content = 1 };
        var b = new Row { Key = "b", Content = 2 };
        var live = new ObservableCollection<Row> { a, b };

        // Same keys + content, swapped order: both instances are reused (moved), not rebuilt.
        Reconcile(live, new Row { Key = "b", Content = 2 }, new Row { Key = "a", Content = 1 });

        Assert.Equal(new[] { "b", "a" }, live.Select(r => r.Key));
        Assert.Same(b, live[0]);
        Assert.Same(a, live[1]);
    }

    [Fact]
    public void MixedChange_ReplacesOnlyChangedRow()
    {
        var a = new Row { Key = "a", Content = 1 };
        var b = new Row { Key = "b", Content = 2 };
        var c = new Row { Key = "c", Content = 3 };
        var live = new ObservableCollection<Row> { a, b, c };
        var bChanged = new Row { Key = "b", Content = 22 };

        // Only "b" changed content; "a" and "c" must keep their instances (containers
        // + in-flight loads), and "b" rebuilds so the fold (e.g. resolved image) paints.
        Reconcile(live,
            new Row { Key = "a", Content = 1 },
            bChanged,
            new Row { Key = "c", Content = 3 });

        Assert.Equal(3, live.Count);
        Assert.Same(a, live[0]);
        Assert.Same(bChanged, live[1]);
        Assert.Same(c, live[2]);
    }

    [Fact]
    public void EmptyTarget_ClearsLive()
    {
        var live = new ObservableCollection<Row>
        {
            new Row { Key = "a", Content = 1 },
            new Row { Key = "b", Content = 2 },
        };

        Reconcile(live);

        Assert.Empty(live);
    }
}
