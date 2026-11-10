using System;
using System.Collections.Generic;
using System.Linq;
using System.Threading.Tasks;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_client_connected_apps;
using uniffi.fauna_media_machine;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// Projects a shared <c>FileVersionSummary</c> (the <c>MediaMachine::file_versions</c>
/// reply) into the <see cref="FileVersionRow"/> the <c>file-version-item</c> DataTemplate
/// binds (media.md § Element IDs).
/// <para>
/// It lives here, not in <c>MediaPage.xaml.cs</c>, for the same reason
/// <see cref="NestTrustFormat"/> does: the XAML project's code-behind is not reachable
/// from <c>FaunaApp.Tests</c>, and the one thing worth pinning is a **unit contract** no
/// e2e assertion covers — the version list's timestamps are never read by
/// <c>test_file_version_history_and_restore</c>, only its sizes.
/// </para>
/// <para>
/// <b>The contract:</b> <c>FileVersionSummary.created_at</c> is epoch <b>milliseconds</b>
/// (<c>libs/fauna-media-machine/src/snapshots.rs</c>), whereas <c>MediaItemSummary.updated_at</c>
/// — rendered as <c>media-item-date</c> on the very same page — is epoch <b>seconds</b>.
/// Both units therefore appear in one DataTemplate pair. Pass <c>created_at</c> to
/// <see cref="ValueFormat.RelativeTime"/> <em>unconverted</em>. (linux multiplies by 1000
/// only because its own helper takes <em>micros</em>; web passes it raw, as we do.)
/// </para>
/// The formatting <em>decisions</em> (relative-time buckets, 1024-unit byte sizes) live
/// once in shared Rust — never hand-roll them per client (priority #2).
/// </summary>
internal static class FileVersionFormat
{
    /// <param name="version">One entry of the oldest→newest history.</param>
    /// <param name="nowMs">Current time, epoch milliseconds — injected rather than read
    /// from the clock so the projection is deterministic under test.</param>
    /// <param name="apps">The connected-apps machine that names a deposited version's
    /// app (<see cref="Author"/>); <c>null</c> paints without it.</param>
    internal static FileVersionRow MapRow(
        FileVersionSummary version, long nowMs, IConnectedAppsMachine? apps = null) => new(
        VersionNum: version.@versionNum,
        Timestamp: ValueFormat.RelativeTime(nowMs, version.@createdAt),
        // size_bytes is i64 on the wire; a bare (ulong) cast of a negative would wrap to
        // ~16 EB. Clamp instead of trusting the projection to never emit one.
        Size: ValueFormat.ByteSize((ulong)Math.Max(0L, version.@sizeBytes)),
        Author: Author(version, apps),
        Pruned: version.@pruned);

    /// <summary>
    /// The <c>file-version-author</c> line. The shared connected-apps machine composes it
    /// (<c>ConnectedAppsMachine::version_author</c>; <c>folder-deposit.md</c> ruling 3):
    /// the recorder alone, or "&lt;app&gt; via &lt;owner&gt;" on a version a connected app
    /// deposited, the app's name read off the roster by its writer key. With no machine,
    /// the recorder alone, or the generic depositor wording on a deposited version —
    /// tui's <c>version_author</c> and linux's do the same. No wording and no writer-key
    /// matching here (priority #2).
    /// </summary>
    internal static string Author(FileVersionSummary version, IConnectedAppsMachine? apps) => apps switch
    {
        { } machine => Strings.Resolve(
            machine.VersionAuthor(version.@authorDisplay, version.@depositedBy)),
        null when version.@depositedBy is not null =>
            Strings.Format("media/version_author_deposited_unknown", version.@authorDisplay),
        null => Strings.Format("media/version_author", version.@authorDisplay),
    };

    /// <summary>The depositors the listed versions name (<c>deposited_by</c>, hex writer
    /// keys) — what <see cref="ReadDepositorRosterAsync"/> asks about.</summary>
    internal static string[] DepositorKeys(IEnumerable<FileVersionSummary> versions) =>
        versions.Select(v => v.@depositedBy).OfType<string>().ToArray();

    /// <summary>
    /// Read the connected-apps roster when naming the listed versions' depositors wants
    /// it — the shared <c>depositor_read_wanted</c>, asked once per key, so a key no roster
    /// will ever name costs one read, never a loop (a roster read at the approve predates
    /// the principal row the nest writes at the app's code redeem). <c>true</c> when the
    /// roster was read, so the caller repaints the rows. tui's <c>depositor_roster_op</c>,
    /// linux's <c>read_depositor_roster</c> and android's <c>readDepositorRoster</c> are
    /// the same shape.
    /// </summary>
    internal static async Task<bool> ReadDepositorRosterAsync(
        IConnectedAppsMachine? apps, string[] depositors)
    {
        if (apps is null || depositors.Length == 0) return false;
        if (!apps.DepositorReadWanted(depositors)) return false;
        await apps.Refresh();
        return true;
    }
}
