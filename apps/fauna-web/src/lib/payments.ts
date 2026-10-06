// The web SPA's whole `payments` glue — the RPC seam, the wasm faces and the
// shared label/amount formatters — in ONE module, so a store-safe build emits
// neither the code nor the chunk.
//
// WHY THIS FILE EXISTS AT ALL, AND WHY ITS CONTENTS ARE NOT WHERE THEY LOOK LIKE
// THEY BELONG. `dynamic-features.md` § Platform-family surface excision fixes the
// web family's excision mechanism as *"a vite define + the isolated-module
// pattern so production builds emit neither code nor chunk"*. The define
// (`__FAUNA_PAYMENTS__`, `vite.config.ts`) folds the branch; the **isolation** is
// what actually removes the surface, and it only works if every payments symbol
// lives in a module nothing else imports. `$lib/rpc`, `$lib/wasm` and
// `$lib/value-format` are all unconditionally in the bundle, so a payments
// function left in one of them ships its own name into the store-safe artifact
// even with every caller folded away — which is criterion 2's failure on web
// (`just web-store-safe-check` greps exactly these face names; see that recipe
// for why the web column's second axis is face names rather than kind strings).
//
// So: the RPC calls came from `$lib/rpc`, `paymentsKnownKinds` /
// `paymentsWebhookUrl` / the tip + status label faces from `$lib/wasm`, and the
// resolved formatters from `$lib/value-format`. Add a new payments surface HERE,
// never back in one of those three.
//
// The only importers may be `$lib/components/payments/*` — the components that
// are themselves loaded from an `if (__FAUNA_PAYMENTS__)` branch. An import from
// anywhere else silently re-attaches this module to the shared graph and the
// witness goes red.

import { rpcCall } from '$lib/rpc';
import { wasmCoreModule } from '$lib/wasm';
import { resolveLocalized, type LocalizedText } from '$lib/i18n/localized';

// ── fauna.payments.* (Pillar 3 client legs — monetization.md § Pillars 2+3) ──
//
// The thin TS seam over the wasm `WsRpcClient.payments*` methods
// (libs/fauna-wasm/src/rpc.rs), themselves over the shared
// `fauna-client-payments` `PaymentsClient` (priority #2). Author side: the
// profile Tiers-tab §4 provider section; buyer side: the
// `subscription-settings` claim redemption. The nest owns validation (unknown
// kind / dangling tier / empty secret are typed error kinds).

/** One configured payment provider (§4 row) — never carries the webhook secret.
 * `last_verified_at`/`last_rejected_at` (epoch seconds, `null` = no evidence yet)
 * feed the evidence-based status badge (`providerStatusLabel`). */
export interface PaymentProvider {
  kind: string;
  tier: string;
  last_verified_at: number | null;
  last_rejected_at: number | null;
}

/** Upsert the author's provider config (`providers.set`; kind + webhook secret + entitled tier). */
export function paymentsProvidersSet(
  secretHex: string,
  kind: string,
  webhookSecret: string,
  tier: string,
): Promise<void> {
  return rpcCall(secretHex, (c) => c.paymentsProvidersSet(kind, webhookSecret, tier) as Promise<void>);
}

/** The author's configured providers (`providers.list`), ascending by kind. */
export function paymentsProvidersList(secretHex: string): Promise<PaymentProvider[]> {
  return rpcCall(secretHex, (c) => c.paymentsProvidersList() as Promise<PaymentProvider[]>);
}

/** Delete the author's config for one provider kind (`providers.remove`; idempotent). */
export function paymentsProvidersRemove(secretHex: string, kind: string): Promise<void> {
  return rpcCall(secretHex, (c) => c.paymentsProvidersRemove(kind) as Promise<void>);
}

/** A redeemed claim: the creator + tier it entitles; `queued` mirrors a queued subscribe. */
export interface PaymentClaimRedeemed {
  author: string; // hex
  tier: string;
  queued: boolean;
}

/** Redeem a post-payment claim code (`claims.redeem`) — binds the entitlement to
 *  this actor; the nest answers a bad code with its own typed claim errors. */
export function paymentsClaimsRedeem(
  secretHex: string,
  code: string,
): Promise<PaymentClaimRedeemed> {
  return rpcCall(secretHex, (c) => c.paymentsClaimsRedeem(code) as Promise<PaymentClaimRedeemed>);
}

/** A freshly minted manual claim code (`claims.mint`). */
export interface PaymentClaimMinted {
  code: string;
  tier: string;
  valid_until: number | null;
}

/** One row of the author's §5 claim-code audit list (`claims.list`). */
export interface PaymentClaim {
  code: string;
  tier: string;
  provider: string; // "manual" for author-minted; the provider kind otherwise
  valid_until: number | null;
  created_at: number;
  redeemed_by: string | null; // hex actor id
  redeemed_at: number | null;
  voided_at: number | null;
}

/** Mint a claim code for a no-API provider already paid out-of-band (bank
 *  transfer, cash, …) — `claims.mint`, always `provider = "manual"`. The nest
 *  rejects a tier that isn't one of the author's own. */
export function paymentsClaimsMint(
  secretHex: string,
  tier: string,
  validUntil?: number,
): Promise<PaymentClaimMinted> {
  // `valid_until` is a `u64` epoch-seconds on the Rust side, which wasm-bindgen
  // types as `bigint`. Convert at the boundary so callers keep a plain `number`
  // (the reply's `valid_until` comes back as a JSON number, so an untyped
  // round-trip here would be a silent asymmetry).
  return rpcCall(
    secretHex,
    (c) =>
      c.paymentsClaimsMint(
        tier,
        validUntil == null ? undefined : BigInt(validUntil),
      ) as Promise<PaymentClaimMinted>,
  );
}

/** The author's own claim codes, newest first (`claims.list`) — the audit
 *  surface for BOTH manually-minted and webhook-minted codes; for the latter
 *  the webhook HTTP response body is the only other delivery channel
 *  (monetization.md § Pillar 3). */
export function paymentsClaimsList(secretHex: string): Promise<PaymentClaim[]> {
  return rpcCall(secretHex, (c) => c.paymentsClaimsList() as Promise<PaymentClaim[]>);
}

// ── the wasm faces (moved out of `$lib/wasm` — see the header) ───────────────

/**
 * Every registered payment-provider kind — the §4 provider form's kind select
 * enumerates these (the shared `fauna-payments` registry the nest's
 * `providers.set` validates against; monetization.md § Pillar 3).
 */
export function paymentsKnownKinds(): string[] {
  return wasmCoreModule().paymentsKnownKinds() as string[];
}

/**
 * The exact URL a creator registers at their provider's dashboard for `kind` —
 * the §4 provider form previews it live as the kind select changes, before the
 * provider is even saved (derived client-side, no nest round-trip).
 *
 * Built from the same `fauna-payments` constant the nest registers its ingress
 * route from, so the SPA never hand-assembles the path (monetization.md
 * § Pillar 3 — Webhook ingress).
 */
export function paymentsWebhookUrl(
  baseUrl: string,
  authorIdHex: string,
  kind: string,
): string {
  return wasmCoreModule().paymentsWebhookUrl(baseUrl, authorIdHex, kind) as string;
}

// ── the resolved formatters (moved out of `$lib/value-format` — see the header) ──
//
// Same contract as every other formatter there: the shared `fauna_core::format`
// decision is made in Rust and this only resolves it against web's i18n table.
// Clients MUST NOT hand-roll the bucketing or the English strings
// (`docs/goal/behavior/value-formatting.md`).

/**
 * A tip amount on the shared sats/msats scale ("21 sats") for the tip total and
 * each tip row's amount. The unit split (sats above 1 sat, msats below) is a
 * shared decision (`fauna_core::format::tip_amount`). See `monetization.md`
 * § Tips.
 */
export function tipAmount(msats: number): string {
  return resolveLocalized(wasmCoreModule().tipAmount(msats) as LocalizedText);
}

/** A tip count with its singular ("1 tip" / "3 tips") — shared so no client
 *  ships "1 tips". */
export function tipCount(count: number): string {
  return resolveLocalized(wasmCoreModule().tipCount(count) as LocalizedText);
}

/**
 * The tip list's "and N more" tail, for a bounded attribution window. `n` comes
 * from the nest's own `has_more` + totals, never from comparing the rendered row
 * count against a cap this client hard-codes.
 */
export function tipMore(n: number): string {
  return resolveLocalized(wasmCoreModule().tipMore(n) as LocalizedText);
}

/**
 * The §5 manual-claims list status badge ("Unredeemed" / "Redeemed" /
 * "Voided") for one claim row. **Redeemed wins over voided** — the decision
 * lives in shared Rust (`fauna_core::format::claim_status_label`), not here,
 * because the two wire booleans are independent and a client branching the
 * other way would silently disagree with its siblings. See monetization.md
 * § Pillar 3.
 */
export function claimStatusLabel(redeemed: boolean, voided: boolean): string {
  return resolveLocalized(wasmCoreModule().claimStatusLabel(redeemed, voided) as LocalizedText);
}

/**
 * The §4 payment-provider status badge ("Configured" / "Verified" / "Error")
 * for one provider row. Evidence-based, never an active probe — derives purely
 * from `PaymentProvider.{last_verified_at,last_rejected_at}` (epoch seconds;
 * `null` = no evidence yet). The decision lives in shared Rust
 * (`fauna_core::format::provider_status_label`), not here. See monetization.md
 * § Pillar 3 → "Provider status — evidence-based, no ping".
 */
export function providerStatusLabel(
  lastVerifiedAt: number | null,
  lastRejectedAt: number | null,
): string {
  return resolveLocalized(
    wasmCoreModule().providerStatusLabel(
      lastVerifiedAt == null ? undefined : BigInt(lastVerifiedAt),
      lastRejectedAt == null ? undefined : BigInt(lastRejectedAt),
    ) as LocalizedText,
  );
}
