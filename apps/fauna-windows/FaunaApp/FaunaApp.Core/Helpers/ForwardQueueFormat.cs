using FaunaApp.Core.Services;
using uniffi.fauna_client_pair;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Shell-side projection of the Nests page's forward-queue block
/// (<c>nests-forward-*</c>; docs/goal/ui/nests.md § Forward queue, ruling in
/// docs/goal/architecture/nest/private-mode.md § Post Forwarding). The queue
/// itself, its control-stripping and its empty-reason folding are shared Rust
/// (<c>fauna_client_pair::ForwardQueueStatus</c>); this only decides what the
/// block paints, mirroring linux's <c>build_forward_queue</c> and tui's
/// <c>push_forward_queue</c> field for field. The reason is partly relay-chosen
/// text, so the XAML paints it through a plain <c>TextBlock.Text</c> — never a
/// <c>RichTextBlock</c>, markup or hyperlink-detecting control.
/// </summary>
internal static class ForwardQueueFormat
{
    /// <summary>The block is painted only while <c>queued &gt; 0</c>; an absent
    /// queue or an empty one carries none of the four ids.</summary>
    internal static bool IsVisible(ForwardQueueStatus? queue) => queue is { queued: > 0 };

    /// <summary><c>nests-forward-queue</c>: the count, plus the stuck half's
    /// what-to-check when any entry is past the retry ceiling.</summary>
    internal static string Summary(ForwardQueueStatus queue)
    {
        var summary = Strings.Format("nests/forward_queue_summary", queue.queued);
        return queue.stuck > 0
            ? $"{summary} {Strings.Format("nests/forward_queue_stuck", queue.stuck)}"
            : summary;
    }

    /// <summary><c>nests-forward-queue-reason</c>: the nest's latest recorded
    /// failure, or <c>null</c> — the element is then absent, never empty.</summary>
    internal static string? Reason(ForwardQueueStatus queue) =>
        string.IsNullOrEmpty(queue.lastError)
            ? null
            : Strings.Format("nests/forward_queue_last_error", queue.lastError);
}
