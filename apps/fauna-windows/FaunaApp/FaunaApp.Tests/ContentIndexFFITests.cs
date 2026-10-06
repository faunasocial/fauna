using Xunit;
using uniffi.fauna_index;

public class ContentIndexFFITests
{
    [Fact]
    public void IndexHandle_RoundTrip()
    {
        var handle = IndexHandle.CreateInRam();

        var doc = new IndexedDoc(
            kind: ContentKind.Mail,
            contentId: new byte[] { 0xAA, 0xBB, 0xCC, 0xDD },
            timestampNs: 1_000,
            senderActorId: null,
            // Stored-but-not-searched carrier field (libs/fauna-index/src/index.rs
            // — "not a search surface"); these round-trips assert the query path,
            // so they carry none.
            secondaryId: null,
            fields: new IndexedField[]
            {
                new(kind: FieldKind.Body, text: "hello from csharp"),
            }
        );
        handle.AddDoc(doc);
        handle.Commit();

        var hits = handle.Query(
            query: "csharp",
            kinds: new[] { ContentKind.Mail },
            range: null,
            limit: 10
        );
        Assert.Single(hits);
        Assert.Equal(ContentKind.Mail, hits[0].kind);
        Assert.Equal(new byte[] { 0xAA, 0xBB, 0xCC, 0xDD }, hits[0].contentId);
    }

    [Fact]
    public void EmptyQuery_YieldsNoHits()
    {
        var handle = IndexHandle.CreateInRam();
        var doc = new IndexedDoc(
            kind: ContentKind.Post,
            contentId: new byte[] { 1, 2, 3 },
            timestampNs: 0,
            senderActorId: null,
            // Stored-but-not-searched carrier field (libs/fauna-index/src/index.rs
            // — "not a search surface"); these round-trips assert the query path,
            // so they carry none.
            secondaryId: null,
            fields: new IndexedField[]
            {
                new(kind: FieldKind.Body, text: "anything"),
            }
        );
        handle.AddDoc(doc);
        handle.Commit();

        var hits = handle.Query("", new[] { ContentKind.Post }, null, 10);
        Assert.Empty(hits);
    }

    [Fact]
    public void KindFilter_ExcludesOtherKinds()
    {
        var handle = IndexHandle.CreateInRam();

        handle.AddDoc(new IndexedDoc(
            kind: ContentKind.Mail,
            contentId: new byte[] { 1 },
            timestampNs: 1_000,
            senderActorId: null,
            // Stored-but-not-searched carrier field (libs/fauna-index/src/index.rs
            // — "not a search surface"); these round-trips assert the query path,
            // so they carry none.
            secondaryId: null,
            fields: new IndexedField[] { new(kind: FieldKind.Body, text: "hello") }
        ));
        handle.AddDoc(new IndexedDoc(
            kind: ContentKind.Post,
            contentId: new byte[] { 2 },
            timestampNs: 2_000,
            senderActorId: null,
            // Stored-but-not-searched carrier field (libs/fauna-index/src/index.rs
            // — "not a search surface"); these round-trips assert the query path,
            // so they carry none.
            secondaryId: null,
            fields: new IndexedField[] { new(kind: FieldKind.Body, text: "hello") }
        ));
        handle.Commit();

        var hits = handle.Query("hello", new[] { ContentKind.Post }, null, 10);
        Assert.Single(hits);
        Assert.Equal(ContentKind.Post, hits[0].kind);
    }
}
