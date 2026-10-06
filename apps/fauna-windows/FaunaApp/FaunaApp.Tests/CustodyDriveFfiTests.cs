using System;
using System.IO;
using System.Linq;
using System.Threading.Tasks;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// The invariant this file pins: <b>the custody ceremony drive
/// (<c>custody_drive</c>) can be called from a thread that holds no tokio
/// runtime</b> — which is every UniFFI caller's situation, on all three
/// UniFFI app families (windows, macOS/iOS, android).
///
/// <para>Why it needs pinning. The drive is fire-and-forget: the shared
/// <c>fauna_client_custody::spawn_drive</c> hands the pass to
/// <c>tokio::spawn</c> and returns. <c>tokio::spawn</c> needs an entered
/// runtime context, and a foreign thread has none — only an
/// <c>async_runtime = "tokio"</c> export enters one while it is polled
/// (<c>docs/goal/architecture/apps/native-async-execution.md</c> § The
/// execution model). The export was the one synchronous function in the custody
/// face, so every call panicked with "there is no reactor running". Each app
/// swallowed that as a best-effort failure, so the drive never ran anywhere and
/// the owner-side receipt line could never converge. On windows the panic also
/// skipped the facet load behind it, so the Devices page's custody section never
/// filled.</para>
///
/// <para>Why here and not a Rust unit test: a Rust test has to supply the
/// runtime context this invariant says the export must not depend on. The
/// generated binding is the real crossing, and it is the same Rust scaffolding
/// Swift and Kotlin call.</para>
/// </summary>
public class CustodyDriveFfiTests
{
    [Fact]
    public async Task CustodyDrive_FromAThreadWithNoTokioRuntime_DoesNotThrow()
    {
        var secret = Enumerable.Repeat((byte)9, 32).ToArray();
        // A unique MLS db per run: the session takes the engine role for its path.
        var dir = Path.Combine(Path.GetTempPath(), $"fauna-custody-drive-{Guid.NewGuid():N}");
        Directory.CreateDirectory(dir);
        try
        {
            // Neither build opens a socket (FfiNestClient.new is offline, and the
            // session is local MLS state), so the drive's own pass simply finds
            // no nest; what is under test is only the call returning.
            using var nest = new FfiNestClient("wss://127.0.0.1:0/ws", secret);
            using var session = nest.ConversationsSession(
                "someone@example.test", secret, Path.Combine(dir, "mls.sqlite"), null,
                Array.Empty<byte[]>());

            // The first poll runs synchronously on this test thread, which holds no
            // tokio runtime — exactly where the synchronous export used to panic.
            var thrown = await Record.ExceptionAsync(() => FaunaFfiMethods.CustodyDrive(nest, secret, session));

            Assert.True(thrown is null,
                "custody_drive must not need an ambient tokio runtime — a UniFFI caller "
                + $"never has one. Threw: {thrown?.GetType().Name}: {thrown?.Message}");
        }
        finally
        {
            // Best-effort: the spawned pass may still hold the sqlite file open.
            try { Directory.Delete(dir, recursive: true); } catch { }
        }
    }
}
