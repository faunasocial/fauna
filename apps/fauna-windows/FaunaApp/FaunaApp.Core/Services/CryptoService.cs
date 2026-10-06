using uniffi.fauna_ffi;

namespace FaunaApp.Core.Services;

/// <summary>
/// Ed25519 keypair holder. All cryptographic operations delegate to the
/// uniffi-generated bindings in <c>libs/fauna-ffi/src/auth.rs</c>, so the
/// byte layouts are guaranteed to match Apple/Android/Linux/Web.
/// </summary>
public sealed class CryptoService : ICryptoService
{
    private byte[]? _secret;
    private byte[]? _actorIdBytes;
    private string? _actorIdHex;

    public string ActorIdHex => _actorIdHex ?? throw new InvalidOperationException("No key loaded");
    public byte[] ActorIdBytes => _actorIdBytes ?? throw new InvalidOperationException("No key loaded");
    public byte[] SecretBytes => _secret ?? throw new InvalidOperationException("No key loaded");
    public bool HasKey => _secret != null;

    public void LoadFromSecret(string secretHex)
    {
        SetSecret(Convert.FromHexString(secretHex));
    }

    public string GenerateKeypair()
    {
        var secret = FaunaFfiMethods.GenerateKeypair();
        SetSecret(secret);
        return Convert.ToHexString(secret).ToLowerInvariant();
    }

    private void SetSecret(byte[] secret)
    {
        _secret = secret;
        _actorIdBytes = FaunaFfiMethods.ActorIdFromSecret(secret);
        _actorIdHex = Convert.ToHexString(_actorIdBytes).ToLowerInvariant();
    }

    public byte[] Sign(byte[] message)
    {
        var secret = _secret ?? throw new InvalidOperationException("No key loaded");
        return FaunaFfiMethods.SignMessage(secret, message);
    }
}
