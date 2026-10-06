namespace FaunaApp.Core.Models;

// The whole plane is the App-Store escape hatch's excision unit on windows
// (dynamic-features.md § Platform-family surface excision). `zaps` is a SUBSET
// member of `payments` and rides the SAME store-safe axis (one C# `PAYMENTS`
// define for the whole family), so a store-safe build's generated C# face has
// no `FfiZapSignerEntry`/`FfiNostrZapSignerClient` at all — this model would not
// compile there even if the define were left on.
#if PAYMENTS

/// <summary>
/// The *Zap signers* control's app model (docs/goal/behavior/monetization.md §
/// Zap receipts — the trust model; docs/goal/ui/nostr.md § Layout &amp; flow item
/// 7) — the NIP-57 trust root a payee designates. Mirrors <c>FfiZapSignerEntry</c>
/// (<c>libs/fauna-ffi/src/nostr_client.rs</c>) one-for-one, so the mapping in
/// <c>NestRpcClient</c> stays a rename rather than a reinterpretation, matching
/// <c>BunkerInvite</c>'s own shape one plane over.
/// </summary>
/// <param name="SignerPubkey">64 lowercase hex — normalized nest-side on write,
/// so this is the form to render and the form to pass back to <c>remove</c>,
/// whatever case the user pasted.</param>
public record ZapSignerEntry(
    long Id,
    string SignerPubkey,
    string Label,
    ulong CreatedAt);

#endif
