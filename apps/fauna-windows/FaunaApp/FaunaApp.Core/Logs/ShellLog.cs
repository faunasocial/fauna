using uniffi.fauna_ffi;
using uniffi.fauna_log;

namespace FaunaApp.Core.Logs;

/// <summary>
/// The windows shell's producer-side bridge into the shared <c>fauna_log</c> ring
/// (observability.md § The emit API). Every message the C# shell wants captured —
/// a displayed error (via <see cref="ViewModels.ViewModelBase"/>), a converted
/// <c>Debug.WriteLine</c>, a meaningful swallow — calls one of these and reaches the
/// SAME ring + on-disk file <c>FaunaFfiMethods.InstallLogging</c> set up at app start,
/// so the Settings → Logs page shows it. The C# twin of android <c>core/ShellLog.kt</c>.
///
/// <para><paramref name="source"/> is the module/VM name; the emitted target is
/// <c>fauna_windows::{source}</c> (the <c>fauna_&lt;platform&gt;::&lt;module&gt;</c>
/// convention web uses, e.g. <c>fauna_web::banner</c>).</para>
///
/// <para>Calls are guarded: if the native ring was never installed (a pre-install
/// producer, a host without the <c>fauna_ffi</c> dll), the emit is swallowed so a
/// producer never crashes — the twin of <c>App.OnLaunched</c>'s best-effort install
/// try/catch.</para>
///
/// <para><b>Redaction</b> (observability.md § Persistence &amp; privacy): pass levels,
/// targets, op-names, and the already-displayed/localized message only — NEVER
/// plaintext bodies, secrets, keys, tokens, or claim codes.</para>
/// </summary>
internal static class ShellLog
{
    internal static void Error(string source, string message) => Emit(LogLevel.Error, source, message);
    internal static void Warn(string source, string message) => Emit(LogLevel.Warn, source, message);
    internal static void Info(string source, string message) => Emit(LogLevel.Info, source, message);
    internal static void Debug(string source, string message) => Emit(LogLevel.Debug, source, message);

    private static void Emit(LogLevel level, string source, string message)
    {
        Tee(level, source, message);
        try { FaunaFfiMethods.LogMessage(level, $"fauna_windows::{source}", message); }
        catch { /* ring not installed (pre-install / no native lib) — never crash a producer */ }
    }

    // ── e2e diagnostic tee ──────────────────────────────────────────────
    //
    // When FAUNA_E2E_AGENT_LOG is set, mirror every shell log line into the SAME
    // file TestAgent writes its per-iteration trace to, through the SAME lock
    // (see E2eTrace). The native ring is the right PRODUCT sink, but a wedge
    // diagnosis is a question about ORDERING between the agent's poll loop and the
    // UI thread, and interleaving two files written by two clocks is what made this
    // class of bug take several sessions to corner. One file, one clock, both
    // sides, each line thread-tagged — which is how a blocked UI thread is told
    // apart from an await that never resumes.
    private static void Tee(LogLevel level, string source, string message)
    {
        if (!E2eTrace.Enabled) return;
        E2eTrace.Write($"{level} {source}: {message}");
    }
}
