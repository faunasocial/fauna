using System;
using System.IO;
using System.Linq;
using System.Runtime.InteropServices;
using System.Text;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Live witness for the app's production secret backend: the shared Rust
/// credential store, reached over the FFI, on its Credential Manager arm — the
/// arm it resolves with no harness credential dir set, which a unit-test run
/// never sets.
///
/// <para>What the app promises about where the identity rests
/// (<c>docs/goal/architecture/apps/common.md</c> § Credential storage, the
/// Windows row) is a statement about the row Windows actually holds, so these
/// read it back through Win32 <c>CredReadW</c> directly — an observer that
/// shares no code with the writer — rather than through the backend's own
/// <c>Get</c>.</para>
///
/// <para>A red names the step that went wrong (convention 6): each step is
/// checked against the row Windows holds, and the message carries the shared
/// store's own warn lines for that row — the errors the backend swallows by
/// contract.</para>
///
/// <para>Safe on any windows session, like the crate's own
/// <c>win_credman::tests::round_trip_and_namespace_sweep</c>: every row lives
/// under a per-run probe namespace, so the real <c>fauna-*</c> namespaces are
/// never touched, and is removed in a <c>finally</c> that checks it is gone —
/// a probe row left behind is the developer's to carry, and enough of them
/// fill the per-user store.</para>
/// </summary>
public class SharedSecretBackendTests
{
    private const uint CredTypeGeneric = 1;
    private const uint CredPersistLocalMachine = 2;

    private static string ProbeNamespace() =>
        $"fauna-credman-cs-probe-{Environment.ProcessId}-{Guid.NewGuid():N}";

    /// <summary>Make sure the shared log ring is installed, so the store's warn
    /// lines reach <see cref="FaunaFfiMethods.LogSnapshot"/>. Idempotent at the
    /// Rust layer, as <c>ShellLogTests</c> relies on.</summary>
    private static void EnsureRingInstalled()
    {
        try { FaunaFfiMethods.InstallLogging(Path.Combine(Path.GetTempPath(), "fauna-shelllog-test")); }
        catch { /* already installed this run — the global subscriber persists */ }
    }

    /// <summary>
    /// Fail with <paramref name="step"/> unless Windows holds a row of exactly
    /// <paramref name="expectedBytes"/> at <paramref name="target"/> (<c>null</c>:
    /// no row).
    /// </summary>
    private static void AssertRow(string target, int? expectedBytes, string step)
    {
        var actual = NativeCredential.Read(target)?.Blob.Length;
        if (actual == expectedBytes) return;
        var logged = FaunaFfiMethods.LogSnapshot()
            .Where(e => e.message.Contains(target))
            .Select(e => $"  [{e.level}] {e.message}")
            .ToList();
        Assert.Fail(
            $"{step}: Credential Manager holds {Describe(actual)} at {target}, expected {Describe(expectedBytes)}. "
            + (logged.Count == 0
                ? "The shared store logged nothing for it."
                : "The shared store logged:" + Environment.NewLine + string.Join(Environment.NewLine, logged)));
    }

    private static string Describe(int? bytes) => bytes is { } n ? $"a {n}-byte row" : "no row";

    /// <summary>Delete the probe row, and make sure it is gone even when the
    /// backend's own delete did not take.</summary>
    private static void RemoveProbeRow(SharedSecretBackend backend, string key, string target)
    {
        backend.Delete(key);
        if (NativeCredential.Read(target) is not null) NativeCredential.Delete(target);
    }

    /// <summary>
    /// A row is one generic credential named <c>"{namespace}/{key}"</c>, its
    /// value the UTF-8 bytes, persisted <c>LOCAL_MACHINE</c> — the persistence
    /// that never roams to the person's other PCs. The grammar is the terminal
    /// app's and the sync agent's, byte for byte, because it is their code.
    /// </summary>
    [Fact]
    public void ARowIsAGenericCredentialThatDoesNotRoam()
    {
        EnsureRingInstalled();
        var ns = ProbeNamespace();
        var backend = new SharedSecretBackend(ns);
        const string key = "fauna/index";
        var target = $"{ns}/{key}";
        const string value = "värde-ä-1";
        try
        {
            Assert.Null(backend.Get(key));
            AssertRow(target, null, "a fresh probe namespace already holds the row");

            backend.Set(key, value);

            AssertRow(target, Encoding.UTF8.GetByteCount(value), "the write did not land");
            Assert.Equal(value, backend.Get(key));
            var row = NativeCredential.Read(target);
            Assert.NotNull(row);
            Assert.Equal(target, row!.TargetName);
            Assert.Equal(CredTypeGeneric, row.Type);
            Assert.Equal(CredPersistLocalMachine, row.Persist);
            Assert.Equal(Encoding.UTF8.GetBytes(value), row.Blob);

            // Replace, not append; and an empty value is a real, empty row.
            backend.Set(key, "value-2");
            Assert.Equal("value-2", backend.Get(key));
            backend.Set(key, "");
            Assert.Equal("", backend.Get(key));

            backend.Delete(key);
            AssertRow(target, null, "the row survived its Delete");
            Assert.Null(backend.Get(key));
            // Deleting an absent row is a quiet no-op.
            backend.Delete(key);
        }
        finally
        {
            RemoveProbeRow(backend, key, target);
        }
    }

    /// <summary>
    /// One value is capped at 2560 bytes, and a write past the cap reports
    /// nothing: the row simply is not there.
    /// </summary>
    [Fact]
    public void AValueOverTheCapDoesNotLand()
    {
        EnsureRingInstalled();
        var ns = ProbeNamespace();
        var backend = new SharedSecretBackend(ns);
        const string key = "fauna/index";
        var target = $"{ns}/{key}";
        var atCap = new string('x', 2560);
        var overCap = new string('x', 2561);
        try
        {
            backend.Set(key, atCap);
            AssertRow(target, 2560, "the at-cap write did not land");
            Assert.Equal(atCap, backend.Get(key));

            // The over-cap step must start from no row: a Delete that did not
            // take would otherwise read back as an over-cap write that did.
            backend.Delete(key);
            AssertRow(target, null, "the at-cap row survived its Delete");

            backend.Set(key, overCap);
            AssertRow(target, null, "the over-cap write landed");
            Assert.Null(backend.Get(key));
        }
        finally
        {
            RemoveProbeRow(backend, key, target);
        }
    }

    /// <summary>One Credential Manager row as Win32 reports it.</summary>
    private sealed record NativeCredential(string TargetName, uint Type, uint Persist, byte[] Blob)
    {
        [StructLayout(LayoutKind.Sequential, CharSet = CharSet.Unicode)]
        private struct CREDENTIALW
        {
            public uint Flags;
            public uint Type;
            public IntPtr TargetName;
            public IntPtr Comment;
            public System.Runtime.InteropServices.ComTypes.FILETIME LastWritten;
            public uint CredentialBlobSize;
            public IntPtr CredentialBlob;
            public uint Persist;
            public uint AttributeCount;
            public IntPtr Attributes;
            public IntPtr TargetAlias;
            public IntPtr UserName;
        }

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredReadW(string target, uint type, uint flags, out IntPtr credential);

        [DllImport("advapi32.dll")]
        private static extern void CredFree(IntPtr buffer);

        [DllImport("advapi32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CredDeleteW(string target, uint type, uint flags);

        /// <summary>Delete the generic credential named <paramref name="target"/> — cleanup only.</summary>
        internal static void Delete(string target) => CredDeleteW(target, CredTypeGeneric, 0);

        /// <summary>The generic credential named <paramref name="target"/>, or <c>null</c> when there is none.</summary>
        internal static NativeCredential? Read(string target)
        {
            if (!CredReadW(target, CredTypeGeneric, 0, out var ptr)) return null;
            try
            {
                var cred = Marshal.PtrToStructure<CREDENTIALW>(ptr);
                var blob = new byte[cred.CredentialBlobSize];
                if (blob.Length > 0) Marshal.Copy(cred.CredentialBlob, blob, 0, blob.Length);
                return new NativeCredential(
                    Marshal.PtrToStringUni(cred.TargetName) ?? "", cred.Type, cred.Persist, blob);
            }
            finally
            {
                CredFree(ptr);
            }
        }
    }
}
