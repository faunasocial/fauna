using System;
using System.Runtime.InteropServices;
using System.Text;
using FaunaApp.Core.Services;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Live witness for the app's production secret backend: the shared Rust
/// Credential Manager arm, reached over the FFI.
///
/// <para>What the app promises about where the identity rests
/// (<c>docs/goal/architecture/apps/common.md</c> § Credential storage, the
/// Windows row) is a statement about the row Windows actually holds, so these
/// read it back through Win32 <c>CredReadW</c> directly — an observer that
/// shares no code with the writer — rather than through the backend's own
/// <c>Get</c>.</para>
///
/// <para>Safe on any windows session, like the crate's own
/// <c>win_credman::tests::round_trip_and_namespace_sweep</c>: every row lives
/// under a per-run probe namespace and is removed in a <c>finally</c>, so the
/// real <c>fauna-*</c> namespaces are never touched.</para>
/// </summary>
public class CredManSecretBackendTests
{
    private const string User = "fauna";
    private const uint CredTypeGeneric = 1;
    private const uint CredPersistLocalMachine = 2;

    private static string ProbeNamespace() =>
        $"fauna-credman-cs-probe-{Environment.ProcessId}-{Guid.NewGuid():N}";

    /// <summary>
    /// A row is one generic credential named <c>"{namespace}/{key}"</c>, its
    /// value the UTF-8 bytes, persisted <c>LOCAL_MACHINE</c> — the persistence
    /// that never roams to the person's other PCs. The grammar is the terminal
    /// app's and the sync agent's, byte for byte, because it is their code.
    /// </summary>
    [Fact]
    public void ARowIsAGenericCredentialThatDoesNotRoam()
    {
        var ns = ProbeNamespace();
        var backend = new CredManSecretBackend(ns);
        const string key = "fauna/index";
        const string value = "värde-ä-1";
        try
        {
            Assert.Null(backend.Get(key, User));
            Assert.Null(NativeCredential.Read($"{ns}/{key}"));

            backend.Set(key, User, value);

            Assert.Equal(value, backend.Get(key, User));
            var row = NativeCredential.Read($"{ns}/{key}");
            Assert.NotNull(row);
            Assert.Equal($"{ns}/{key}", row!.TargetName);
            Assert.Equal(CredTypeGeneric, row.Type);
            Assert.Equal(CredPersistLocalMachine, row.Persist);
            Assert.Equal(Encoding.UTF8.GetBytes(value), row.Blob);

            // Replace, not append; and an empty value is a real, empty row.
            backend.Set(key, User, "value-2");
            Assert.Equal("value-2", backend.Get(key, User));
            backend.Set(key, User, "");
            Assert.Equal("", backend.Get(key, User));

            backend.Delete(key, User);
            Assert.Null(backend.Get(key, User));
            Assert.Null(NativeCredential.Read($"{ns}/{key}"));
            // Deleting an absent row is a quiet no-op.
            backend.Delete(key, User);
        }
        finally
        {
            backend.Delete(key, User);
        }
    }

    /// <summary>
    /// One value is capped at 2560 bytes, and a write past the cap reports
    /// nothing: the row simply is not there.
    /// </summary>
    [Fact]
    public void AValueOverTheCapDoesNotLand()
    {
        var ns = ProbeNamespace();
        var backend = new CredManSecretBackend(ns);
        const string key = "fauna/index";
        var atCap = new string('x', 2560);
        var overCap = new string('x', 2561);
        try
        {
            backend.Set(key, User, atCap);
            Assert.Equal(atCap, backend.Get(key, User));
            backend.Delete(key, User);

            backend.Set(key, User, overCap);
            Assert.Null(backend.Get(key, User));
        }
        finally
        {
            backend.Delete(key, User);
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
