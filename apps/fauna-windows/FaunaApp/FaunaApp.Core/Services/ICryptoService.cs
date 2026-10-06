namespace FaunaApp.Core.Services;

public interface ICryptoService
{
    string ActorIdHex { get; }
    byte[] ActorIdBytes { get; }
    byte[] SecretBytes { get; }
    bool HasKey { get; }
    void LoadFromSecret(string secretHex);
    string GenerateKeypair();

    /// <summary>
    /// Sign arbitrary bytes (e.g. a BARE-encoded post body) with the loaded
    /// Ed25519 secret. Returns the raw 64-byte signature.
    /// </summary>
    byte[] Sign(byte[] message);
}
