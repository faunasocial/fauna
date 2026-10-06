using System.Diagnostics;
using System.Runtime.InteropServices;
using System.Text;

namespace FauiBridge;

/// <summary>
/// Answers one question the bridge could not previously ask: <b>which live app
/// processes own this session's data dir?</b>
///
/// <para><see cref="SessionManager.Quit"/> used to verify "the handle I hold has
/// exited", which is a strictly weaker claim. Windows' <c>recover()</c> deliberately
/// relaunches against the SAME <c>FAUNA_E2E_DATA_DIR</c> and credential store
/// (<c>drivers/windows.py</c>), so ANY surviving instance — not just the one the
/// bridge started — keeps the per-account instance lock and pushes the relaunched
/// process into the <c>[launch-collision] … already served by a live instance</c>
/// branch, where it never reaches <c>TestAgent.Configure()</c> and therefore never
/// polls commands nor publishes state. One such survivor loses every remaining test
/// module in the session (21/40 tests, six whole files, in one batch).</para>
///
/// <para><b>Why the data dir is the identity, not the process name.</b> A development
/// machine runs many builds and several e2e sessions concurrently, so "kill every
/// <c>FaunaApp.exe</c>" would murder a parallel run — which the project's
/// process-safety rule forbids. <c>FAUNA_E2E_DATA_DIR</c> is a per-driver-instance
/// <c>mkdtemp</c>, so a process carrying THIS session's value is provably this
/// session's own — and every other session's app is provably not.</para>
///
/// <para><b>Why the environment block rather than the app's cooperation.</b> A pid
/// file written by the app would only cover instances that got far enough to write
/// it, and the instances this hunts are precisely the ones stranded early (in the
/// account chooser). The OS's copy of the process environment is written by the
/// kernel at creation time and is true of every process, however wedged. The read is
/// the documented PEB walk (<c>NtQueryInformationProcess</c> →
/// <c>PEB.ProcessParameters</c> → <c>Environment</c>) plus
/// <c>ReadProcessMemory</c>; it needs no privilege beyond same-user, and no package
/// reference (<c>System.Management</c>/WMI would be a new third-party dependency and
/// still could not read environment variables).</para>
///
/// <para>Every failure mode is "report what we could read": a process that exits
/// mid-scan, refuses <c>OpenProcess</c>, or is a bitness/layout we cannot walk is
/// skipped rather than throwing. A scan that cannot see a survivor degrades to the
/// old handle-only verification, never to a crash in teardown.</para>
/// </summary>
static class ProcessScan
{
    /// <summary>A live app process launched against the data dir that was scanned.</summary>
    internal readonly record struct AppInstance(
        int Pid,
        int ParentPid,
        string? Epoch,
        string StartedAt);

    /// <summary>
    /// Every live process named after <paramref name="exePath"/> whose
    /// <c>FAUNA_E2E_DATA_DIR</c> is <paramref name="dataDir"/>, newest launch last.
    /// Empty when the data dir is unknown (a launch that never isolated one) — the
    /// caller then keeps its handle-only verification rather than scanning the box.
    /// </summary>
    public static IReadOnlyList<AppInstance> OwnersOfDataDir(string? exePath, string? dataDir)
    {
        if (string.IsNullOrWhiteSpace(exePath) || string.IsNullOrWhiteSpace(dataDir))
        {
            return Array.Empty<AppInstance>();
        }

        var wanted = Normalize(dataDir);
        if (wanted is null) return Array.Empty<AppInstance>();

        var name = Path.GetFileNameWithoutExtension(exePath);
        if (string.IsNullOrEmpty(name)) return Array.Empty<AppInstance>();

        var found = new List<AppInstance>();
        Process[] candidates;
        try { candidates = Process.GetProcessesByName(name); }
        catch { return Array.Empty<AppInstance>(); }

        foreach (var p in candidates)
        {
            try
            {
                var env = ReadEnvironment(p.Id);
                if (env is null) continue;
                if (!env.TryGetValue("FAUNA_E2E_DATA_DIR", out var theirs)) continue;
                if (!string.Equals(Normalize(theirs), wanted, StringComparison.OrdinalIgnoreCase)) continue;

                env.TryGetValue("FAUNA_E2E_SESSION_EPOCH", out var epoch);
                var started = "?";
                try { started = p.StartTime.ToString("HH:mm:ss.fff"); } catch { }
                found.Add(new AppInstance(p.Id, ParentPidOf(p.Id), epoch, started));
            }
            catch { /* a process that vanished or refused inspection is not a survivor we can act on */ }
            finally { try { p.Dispose(); } catch { } }
        }

        found.Sort((a, b) => string.CompareOrdinal(a.StartedAt, b.StartedAt));
        return found;
    }

    /// <summary>One-line-per-instance rendering for the bridge's stderr and the
    /// python side's failure report: pid, who started it, and which session epoch it
    /// believes it belongs to — the three facts that identify an untracked survivor.</summary>
    public static string Describe(IReadOnlyList<AppInstance> instances) =>
        instances.Count == 0
            ? "(none)"
            : string.Join("; ", instances.Select(i =>
                $"pid {i.Pid} (parent {i.ParentPid}, epoch {i.Epoch ?? "unset"}, started {i.StartedAt})"));

    private static string? Normalize(string? path)
    {
        if (string.IsNullOrWhiteSpace(path)) return null;
        try { return Path.GetFullPath(path).TrimEnd('\\', '/'); }
        catch { return path.TrimEnd('\\', '/'); }
    }

    // ── the PEB walk ────────────────────────────────────────────────────────

    private const int ProcessBasicInformation = 0;
    private const int PROCESS_QUERY_INFORMATION = 0x0400;
    private const int PROCESS_VM_READ = 0x0010;

    // 64-bit PEB / RTL_USER_PROCESS_PARAMETERS offsets. Identical on x64 and ARM64
    // (both LLP64 with 8-byte pointers), which is the only pair this bridge runs on.
    private const int PebProcessParametersOffset = 0x20;
    private const int ParamsEnvironmentOffset = 0x80;

    /// <summary>How much of an environment block we are willing to read before
    /// giving up. Real blocks are a few KB; the cap only bounds a corrupt read.</summary>
    private const int EnvironmentReadCapBytes = 512 * 1024;

    private const int EnvironmentChunkBytes = 4096;

    [StructLayout(LayoutKind.Sequential)]
    private struct ProcessBasicInfo
    {
        public IntPtr ExitStatus;
        public IntPtr PebBaseAddress;
        public IntPtr AffinityMask;
        public IntPtr BasePriority;
        public IntPtr UniqueProcessId;
        public IntPtr InheritedFromUniqueProcessId;
    }

    [DllImport("ntdll.dll")]
    private static extern int NtQueryInformationProcess(
        IntPtr handle, int infoClass, ref ProcessBasicInfo info, int size, out int returned);

    [DllImport("kernel32.dll", SetLastError = true)]
    private static extern IntPtr OpenProcess(int access, bool inherit, int pid);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool CloseHandle(IntPtr handle);

    [DllImport("kernel32.dll", SetLastError = true)]
    [return: MarshalAs(UnmanagedType.Bool)]
    private static extern bool ReadProcessMemory(
        IntPtr handle, IntPtr address, byte[] buffer, IntPtr size, out IntPtr read);

    /// <summary>The pid that created <paramref name="pid"/>, or 0 if unknowable.
    /// Note this is the creator recorded at birth: it is NOT re-pointed when the
    /// parent dies, so an orphan still names the process that started it — which is
    /// exactly what identifies an untracked spawner.</summary>
    private static int ParentPidOf(int pid)
    {
        var h = OpenProcess(PROCESS_QUERY_INFORMATION, false, pid);
        if (h == IntPtr.Zero) return 0;
        try
        {
            var pbi = default(ProcessBasicInfo);
            if (NtQueryInformationProcess(h, ProcessBasicInformation, ref pbi, Marshal.SizeOf(pbi), out _) != 0)
                return 0;
            return (int)pbi.InheritedFromUniqueProcessId;
        }
        catch { return 0; }
        finally { CloseHandle(h); }
    }

    /// <summary>The process's environment as the kernel recorded it at creation,
    /// or null if it could not be read.</summary>
    private static Dictionary<string, string>? ReadEnvironment(int pid)
    {
        var h = OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, false, pid);
        if (h == IntPtr.Zero) return null;
        try
        {
            var pbi = default(ProcessBasicInfo);
            if (NtQueryInformationProcess(h, ProcessBasicInformation, ref pbi, Marshal.SizeOf(pbi), out _) != 0)
                return null;
            if (pbi.PebBaseAddress == IntPtr.Zero) return null;

            var paramsPtr = ReadPointer(h, pbi.PebBaseAddress + PebProcessParametersOffset);
            if (paramsPtr == IntPtr.Zero) return null;
            var envPtr = ReadPointer(h, paramsPtr + ParamsEnvironmentOffset);
            if (envPtr == IntPtr.Zero) return null;

            var block = ReadUntilDoubleNull(h, envPtr);
            return block is null ? null : Parse(block);
        }
        catch { return null; }
        finally { CloseHandle(h); }
    }

    private static IntPtr ReadPointer(IntPtr handle, IntPtr address)
    {
        var buf = new byte[IntPtr.Size];
        if (!ReadProcessMemory(handle, address, buf, IntPtr.Size, out var read) || (int)read != IntPtr.Size)
            return IntPtr.Zero;
        return (IntPtr)BitConverter.ToInt64(buf, 0);
    }

    /// <summary>
    /// Read the UTF-16 environment block chunk by chunk until its terminating empty
    /// string (four zero bytes at an even offset).
    ///
    /// <para>Chunked rather than one big read because the block's true length is not
    /// reliably available: <c>RTL_USER_PROCESS_PARAMETERS.EnvironmentSize</c> sits at
    /// a build-dependent offset, and an over-long single read that runs off the end of
    /// the mapping fails <i>entirely</i> rather than partially — turning a readable
    /// block into "unknown". Stopping at the first unreadable chunk keeps whatever was
    /// already decoded.</para>
    /// </summary>
    private static byte[]? ReadUntilDoubleNull(IntPtr handle, IntPtr start)
    {
        var acc = new List<byte>(EnvironmentChunkBytes * 2);
        var offset = 0;
        while (offset < EnvironmentReadCapBytes)
        {
            var n = ReadUpTo(handle, start + offset, EnvironmentChunkBytes, out var buf);
            if (n <= 0) break;
            var scanFrom = Math.Max(0, acc.Count - 3);
            acc.AddRange(buf.AsSpan(0, n).ToArray());
            var end = IndexOfTerminator(acc, scanFrom);
            if (end >= 0) return acc.GetRange(0, end).ToArray();
            offset += n;
        }
        return acc.Count > 0 ? acc.ToArray() : null;
    }

    /// <summary>
    /// Read as much as possible at <paramref name="address"/>, halving the request
    /// until it succeeds. <c>ReadProcessMemory</c> fails the WHOLE call when any part
    /// of the range is unmapped, so a chunk that happens to straddle the end of the
    /// block's allocation reads nothing at all — truncating the environment right
    /// where the interesting variables might be. Halving turns that into a short
    /// read, which is the honest answer.
    ///
    /// <para>The failure this guards is asymmetric and worth the extra call: a
    /// truncated block means <c>FAUNA_E2E_DATA_DIR</c> is not found, which means a
    /// live survivor is reported as "not an owner" — the exact bug this whole file
    /// exists to fix, wearing a false negative.</para>
    /// </summary>
    private static int ReadUpTo(IntPtr handle, IntPtr address, int size, out byte[] buffer)
    {
        for (var want = size; want >= 8; want /= 2)
        {
            buffer = new byte[want];
            if (ReadProcessMemory(handle, address, buffer, want, out var read) && (int)read > 0)
                return (int)read;
        }
        buffer = Array.Empty<byte>();
        return 0;
    }

    /// <summary>Offset of the UTF-16 empty string that ends the block, or -1.</summary>
    private static int IndexOfTerminator(List<byte> bytes, int from)
    {
        for (var i = from + (from % 2); i + 3 < bytes.Count; i += 2)
        {
            if (bytes[i] == 0 && bytes[i + 1] == 0 && bytes[i + 2] == 0 && bytes[i + 3] == 0)
                return i;
        }
        return -1;
    }

    private static Dictionary<string, string> Parse(byte[] block)
    {
        var map = new Dictionary<string, string>(StringComparer.OrdinalIgnoreCase);
        foreach (var entry in Encoding.Unicode.GetString(block).Split('\0'))
        {
            if (entry.Length == 0) continue;
            // Skip the "=C:=C:\dir" per-drive cwd entries, whose name is empty.
            var eq = entry.IndexOf('=', 1);
            if (eq <= 0) continue;
            map[entry[..eq]] = entry[(eq + 1)..];
        }
        return map;
    }
}
