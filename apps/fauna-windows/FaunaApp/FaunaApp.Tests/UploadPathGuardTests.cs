using Xunit;
using FaunaApp.Core.Media;

namespace FaunaApp.Tests;

// ── Media Upload's empty-path precondition ──
//
// The windows peer of linux's `an_upload_with_an_empty_path_says_so_instead_of_
// no_opping` (apps/fauna-linux/src/views/media/mod.rs) and of tui's
// identically-named pin. A live user pressed tui's Upload with an empty box,
// saw nothing happen, and reported the upload feature as missing entirely;
// linux and windows carried the identical silent `return`. windows was the last
// app still carrying it.
//
// docs/goal/ui/media.md § Client glue: "`upload-button` with an empty path box |
// Surface `media.file_required` ("Choose a file first…") on `error-message` —
// never a silent no-op, which presents as a dead button."
//
// The precondition lives in FaunaApp.Core rather than in MediaPage.xaml.cs
// purely so it is testable without a WinUI window — FaunaApp.Tests references
// FaunaApp.Core alone, never the app project (the DayCellClickArbiter
// precedent, extracted from EventsPage.xaml.cs for exactly this reason).

public class UploadPathGuardTests
{
    /// Pressing Upload with an empty path box must SAY so, not no-op.
    ///
    /// Whitespace counts as empty — it is what a stray space in the box looks
    /// like to a user who thinks they typed a path.
    [Fact]
    public void AnUploadWithAnEmptyPath_SaysSo_InsteadOfNoOpping()
    {
        Assert.Null(UploadPathGuard.UploadPathOf("   "));
        Assert.Null(UploadPathGuard.UploadPathOf(""));
        Assert.Null(UploadPathGuard.UploadPathOf(null));

        // The picker-flavoured wording, NOT tui's typed-path `file_path_required`:
        // windows has a real picker, so picker language is the true one here (the
        // split mirrors the existing choose_file / type_file_path pair). A future
        // "unification" onto tui's wording would tell a windows user to type a
        // path into a box the picker fills for them.
        Assert.Equal("media/file_required", UploadPathGuard.FileRequiredKey);
    }

    /// A real path is trimmed and passed straight through to the upload glue.
    [Fact]
    public void ARealPath_IsTrimmedAndPassedThrough()
    {
        Assert.Equal(
            @"C:\pics\photo.png",
            UploadPathGuard.UploadPathOf(@"  C:\pics\photo.png  "));
    }
}
