using FaunaApp.Core.Helpers;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The presence rule for the windows test-message shims — the global
/// <c>error-message</c>/<c>warning-message</c>/<c>info-message</c> TextBoxes on
/// <c>MainPage.xaml</c> and the per-page 1×1 <c>ErrorTextMirror</c>/
/// <c>WarningTextMirror</c> TextBlocks on 14 pages.
/// <para>
/// Why this rule needs a home and a test: those shims used to be **unconditionally
/// on-screen** (a 1×1 TextBox, or a TextBlock whose empty sentinel was a literal
/// <c>" "</c>), while carrying the very same <c>AutomationId</c> as the page's own
/// error InfoBar. The FlaUI bridge's <c>IsVisible</c> resolves an id window-wide
/// and checks only <c>!IsOffscreen</c>, so an always-on-screen shim made
/// <c>is_visible("error-message")</c> read <b>true on a clean page</b> — the
/// negative assertion that opens every "no error at page start" e2e was therefore
/// unobservable, and the whole class of tests silently untestable
/// (<c>test_conversations_compose_error.py</c>, both cases).
/// </para>
/// <para>
/// The rule that fixes it — a shim is in the accessibility tree <b>iff</b> it
/// carries a message — is what linux gets structurally (its finder prunes
/// non-showing widgets, <c>automation/find.rs::is_showing</c>) and what tui/web get
/// by building their error surface hidden. The whitespace clause is the part worth
/// pinning: the mirrors' historical "empty" value is a SPACE, not <c>""</c>, so a
/// plain <c>IsNullOrEmpty</c> check would keep every one of them on-screen and fix
/// nothing.
/// </para>
/// </summary>
public class MessageShimTests
{
    [Fact]
    public void A_message_shows_the_shim()
    {
        Assert.True(MessageShim.ShouldShow("nest rejected fauna.email.send"));
    }

    [Fact]
    public void No_message_hides_the_shim()
    {
        Assert.False(MessageShim.ShouldShow(null));
        Assert.False(MessageShim.ShouldShow(""));
    }

    [Fact]
    public void The_historical_space_sentinel_hides_the_shim()
    {
        // `Text=" "` is the literal XAML default on all 24 per-page mirrors and
        // what `UpdateTestMessages` wrote on clear (`error ?? " "`). Treating it as
        // a message is exactly the bug: it is the absence of one.
        Assert.False(MessageShim.ShouldShow(" "));
        Assert.False(MessageShim.ShouldShow("\t"));
        Assert.False(MessageShim.ShouldShow("   "));
    }

    [Fact]
    public void Surrounding_whitespace_does_not_hide_a_real_message()
    {
        Assert.True(MessageShim.ShouldShow("  no key package published  "));
    }
}
