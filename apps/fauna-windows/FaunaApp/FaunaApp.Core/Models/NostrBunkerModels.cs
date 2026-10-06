namespace FaunaApp.Core.Models;

/// <summary>
/// The Nostr Connect / NIP-46 bunker invite's app model —
/// deliberately NOT in <c>BridgeInfo.cs</c>: this rides the User-class, caller-scoped
/// <c>fauna.nostr.bunker.*</c> kinds, not <c>fauna.bridges.*</c>, because it is "a
/// roster with mint/revoke verbs, not a settings blob" (docs/goal/ui/nostr.md §
/// The nest as the user's NIP-46 signer → Control plane).
///
/// <para>Both records mirror the FFI shapes in <c>libs/fauna-ffi/src/nostr_client.rs</c>
/// one-for-one (which in turn mirror <c>fauna_protocol::nostr</c>), so the mapping in
/// <c>NestRpcClient</c> stays a rename rather than a reinterpretation.</para>
///
/// <para>User-facing vocabulary is "Nostr Connect" / "Connected apps" / "Disconnect" —
/// never "capability" or "grant" (§ The nest as the user's NIP-46 signer, data-shape
/// paragraph). These are authorizations the NEST enforces per request; no key material
/// ever reaches the app, which is exactly why they are not the sealed-grant class.</para>
/// </summary>
/// <summary>The reply to minting a connect invite — the <b>single one-time reveal</b>
/// of the connect string (<c>bunker://&lt;signer-pubkey&gt;?relay=…&amp;secret=…</c>).
/// The secret is single-use with a hard-coded TTL and is never retrievable again, so
/// the UI must show it immediately and say so.</summary>
public record BunkerInvite(
    long ConnectionId,
    string ConnectString,
    string SignerPubkey,
    ulong ExpiresAt);
