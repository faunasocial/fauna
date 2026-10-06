using System;
using System.Collections.Generic;
using System.IO;
using System.Text.Json;
using FaunaApp.Core.Logs;

namespace FaunaApp.Core.Services;

/// <summary>
/// The raw key/value store the logical-keyed <see cref="LogicalSecretStore"/>
/// sits on. Two implementations, exactly mirroring linux's
/// <c>fauna_credential_store::SecretStoreBackend</c>:
///
/// <list type="bullet">
///   <item><description><b>Credential Manager</b> —
///     <see cref="CredManSecretBackend"/>, the shared Rust arm over the FFI.
///     Production: generic credentials that do not roam.</description></item>
///   <item><description><b>File</b> — a flat JSON map, selected by the
///     <c>FAUNA_E2E_CREDENTIAL_DIR</c> environment variable. This is what lets an
///     e2e seed a whole multi-account registry before launch with no app code,
///     and keeps the test suite out of the dev machine's real Credential
///     Manager.</description></item>
/// </list>
///
/// <para>Keys are the <b>native</b> (already-resolved) names — <see cref="SecretKeyMap"/>
/// does the logical→native translation above this seam.</para>
/// </summary>
internal interface ISecretBackend
{
    string? Get(string resource, string user);
    void Set(string resource, string user, string value);
    void Delete(string resource, string user);
}

/// <summary>
/// File-backed backend for E2E: a flat <c>{native_key: value}</c> JSON map at
/// <c>{FAUNA_E2E_CREDENTIAL_DIR}/{namespace}.json</c>. The linux twin is
/// <c>cred_file_read</c>/<c>cred_file_write</c> (same path convention, same flat
/// map), so <c>tests/common/accounts.py::build_registry_seed</c> produces a file
/// both apps can read.
///
/// <para>Keyed by Resource alone (the UserName is ignored): every logical key
/// resolves to a distinct Resource, so Resource is already unique. This matches
/// linux, whose file map is keyed by the libsecret <c>account</c> attribute
/// alone.</para>
///
/// <para>Writes are best-effort and swallowed, matching the store contract the
/// shared crate documents (<c>set</c> failures are swallowed by every platform
/// store; durability is proven by read-back where it matters).</para>
/// </summary>
internal sealed class FileSecretBackend : ISecretBackend
{
    private readonly string _path;
    private readonly object _lock = new();

    internal FileSecretBackend(string dir, string ns)
    {
        _path = Path.Combine(dir, $"{ns}.json");
    }

    private Dictionary<string, string> Read()
    {
        try
        {
            if (!File.Exists(_path)) return new Dictionary<string, string>();
            var json = File.ReadAllText(_path);
            return JsonSerializer.Deserialize<Dictionary<string, string>>(json)
                   ?? new Dictionary<string, string>();
        }
        catch
        {
            // A corrupt/absent credential file degrades to "no credentials"
            // (→ onboarding), never a crash. Same as linux's `unwrap_or_default`.
            return new Dictionary<string, string>();
        }
    }

    private void Write(Dictionary<string, string> map)
    {
        try
        {
            Directory.CreateDirectory(Path.GetDirectoryName(_path)!);
            File.WriteAllText(_path, JsonSerializer.Serialize(map,
                new JsonSerializerOptions { WriteIndented = true }));
        }
        catch (Exception ex)
        {
            ShellLog.Error("FileSecretBackend", $"credential file write failed: {ex.Message}");
        }
    }

    public string? Get(string resource, string user)
    {
        lock (_lock)
        {
            return Read().TryGetValue(resource, out var v) ? v : null;
        }
    }

    public void Set(string resource, string user, string value)
    {
        lock (_lock)
        {
            var map = Read();
            map[resource] = value;
            Write(map);
        }
    }

    public void Delete(string resource, string user)
    {
        lock (_lock)
        {
            var map = Read();
            if (map.Remove(resource)) Write(map);
        }
    }
}
