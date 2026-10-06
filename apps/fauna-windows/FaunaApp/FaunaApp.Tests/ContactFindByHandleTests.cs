using FaunaApp.Core.ViewModels;
using Xunit;
using static FaunaApp.Core.ViewModels.ContactsViewModel;

namespace FaunaApp.Tests;

/// <summary>
/// The <c>contact-actor-id-lookup</c> federated find-user path is single-sourced in
/// shared Rust (<c>fauna_core::resolve::classify_recipient</c> +
/// <c>fauna.nest.resolve</c> / <c>fauna.actor.by_handle</c>), consumed via the
/// <c>libs/fauna-ffi/src/resolve.rs</c> UniFFI seam — windows was the LONE client
/// without the handle branch (contacts.md § Where logic lives → Contact lookup by
/// handle). These call the REAL <c>classifyRecipient</c> export (native <c>fauna_ffi</c>
/// dll loads in the test host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>)
/// through <see cref="ContactsViewModel.ClassifyRecipientInput"/>, the windows classify
/// → branch seam. The two network hops (<c>resolve_nest</c> → <c>resolve_handle</c>) are
/// live anonymous connections not exercisable from a unit test — like the roster filter,
/// the unit-testable value is the classify-and-branch decision, which is exactly what
/// drives which resolve (if any) runs.
/// </summary>
public class ContactFindByHandleTests
{
    [Fact]
    public void ClassifyRecipientInput_Handle_SplitsUserAndDomain()
    {
        // A user@domain input takes the handle branch (resolve_nest → resolve_handle),
        // carrying the split user + domain the two-hop resolve consumes.
        var c = ClassifyRecipientInput("alice@example.com");
        Assert.Equal(RecipientKind.Handle, c.Kind);
        Assert.Equal("alice", c.User);
        Assert.Equal("example.com", c.Domain);
        Assert.Equal("", c.ActorId);
    }

    [Fact]
    public void ClassifyRecipientInput_ActorId_LowercasesHexAndKeepsDirectPath()
    {
        // A 64-hex actor-id takes the direct roster path (no network); the classifier
        // normalizes to lowercase, the form used as the roster key.
        var c = ClassifyRecipientInput(new string('A', 64));
        Assert.Equal(RecipientKind.ActorId, c.Kind);
        Assert.Equal(new string('a', 64), c.ActorId);
        Assert.Equal("", c.User);
        Assert.Equal("", c.Domain);
    }

    [Fact]
    public void ClassifyRecipientInput_Invalid_SurfacesFindError()
    {
        // Neither 64-hex nor user@domain → invalid (the branch that sets
        // contact-find-error). 64 non-hex chars with no `@` is still invalid.
        Assert.Equal(RecipientKind.Invalid, ClassifyRecipientInput("justtext").Kind);
        Assert.Equal(RecipientKind.Invalid, ClassifyRecipientInput(new string('z', 64)).Kind);
        Assert.Equal(RecipientKind.Invalid, ClassifyRecipientInput("").Kind);
        Assert.Equal(RecipientKind.Invalid, ClassifyRecipientInput(null).Kind);
    }

    [Fact]
    public void ClassifyRecipientInput_TrimsSurroundingWhitespace()
    {
        // The field text is trimmed before classification (a pasted handle with
        // surrounding spaces still resolves).
        var c = ClassifyRecipientInput("  bob@other.org  ");
        Assert.Equal(RecipientKind.Handle, c.Kind);
        Assert.Equal("bob", c.User);
        Assert.Equal("other.org", c.Domain);
    }

    [Fact]
    public void ClassifyRecipientInput_MultiDomainHandle_CarriesSecondaryDomainForEcho()
    {
        // Multi-domain contract (mail-multidomain.md § Multi-domain handles): on a
        // deployment serving domain1 (primary) + domain2, a typed `bob@domain2` must
        // carry the SECONDARY domain, because LookUpRecipientAsync threads c.Domain into
        // the FFI resolve_handle(node_url, handle, domain) so the nest echoes `domain2`
        // in slot 3 and windows displays `bob@domain2` (parity with web/linux/android) —
        // rather than reporting the identity/primary domain (the pre-thread None
        // behaviour). This pins the exact value the one-call-site follow-up threads; the
        // nest-side echo itself is proven nest-side (not client-unit-reachable — the
        // resolve is a live anonymous network hop).
        var c = ClassifyRecipientInput("bob@domain2.example");
        Assert.Equal(RecipientKind.Handle, c.Kind);
        Assert.Equal("bob", c.User);
        Assert.Equal("domain2.example", c.Domain);
    }
}
