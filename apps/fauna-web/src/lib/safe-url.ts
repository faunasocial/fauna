/**
 * Guard for nest-supplied URLs the SPA hands to `window.location` / `window.open`.
 *
 * The bridge OAuth `redirect_url` (`routes/bridges/+page.svelte`) and the
 * subscription `payment_url` (`routes/profile/[[actorId]]/+page.svelte`) are both
 * chosen by the home nest, so a compromised or spoofed nest can put any string in
 * them. The browser already refuses to *execute* a `javascript:` / `data:` URL via
 * these navigation sinks and the payment open already sets `noopener`, so this is
 * the anti-phishing-redirect guard (the F-CL2 client-trust class) —
 * NOT an XSS fix: we refuse to *navigate* the user to a non-`https:` destination
 * rather than relay them to an attacker-chosen scheme. This is the residual of the
 * web SPA's documented channel-binding exemption (`docs/goal/architecture/security.md`
 * § Transport trust: the browser validates TLS itself, so the SPA keeps no disk pin
 * store — but it must still refuse to forward the user to an untrusted scheme).
 *
 * Returns true iff `url` parses as an absolute URL with the `https:` scheme. Both
 * destinations are external services (an OAuth provider, a payment processor) that
 * are always https in practice, so an https-only allowlist needs no localhost
 * carve-out — unlike the stored nest URL, which has a same-origin dev fallback.
 */
export function isSafeNavUrl(url: string | null | undefined): boolean {
  if (!url) return false;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  return parsed.protocol === 'https:';
}

/** True for the hosts a browser treats as a secure context over plain http:
 * `localhost`, `*.localhost`, the 127.0.0.0/8 loopback block, and `::1`.
 * `URL.hostname` lowercases ASCII and brackets IPv6, so the comparisons are
 * exact. `0.0.0.0` is a bind address, never a client locator, so it is excluded. */
function isLoopbackHostname(hostname: string): boolean {
  if (hostname === 'localhost' || hostname.endsWith('.localhost')) return true;
  if (hostname === '[::1]') return true;
  return /^127(?:\.\d{1,3}){3}$/.test(hostname);
}

/**
 * Guard for the stored nest URL — the nest API base the SPA talks to
 * (`$lib/api.ts` `storedNestUrlOrNull()` / `nodeUrl()`). The value is persisted
 * in browser storage, so a spoofed value could silently point the client at an
 * attacker-controlled origin; an untrusted value is ignored and `nodeUrl()`
 * falls back to `window.location.origin`.
 *
 * Accepts iff `url` parses as an absolute URL that is either `https:` (the
 * production / LAN-nest scheme — even self-signed LAN nests serve TLS off the
 * floor cert, see `docs/goal/architecture/security.md` § Transport trust) or
 * `http:` to a loopback host (the browser secure-context dev/e2e carve-out:
 * `just web-dev` and the e2e suite serve the nest at `http://127.0.0.1:<port>` /
 * `http://localhost:<port>`). Everything else — http: to a non-loopback host
 * (the phishing/MITM-relay residual this guard closes), javascript:/data:/file:,
 * relative, malformed, or nullish — is rejected. This is the web SPA's residual
 * hardening for its documented channel-binding exemption.
 */
export function isSafeNodeUrl(url: string | null | undefined): boolean {
  if (!url) return false;
  let parsed: URL;
  try {
    parsed = new URL(url);
  } catch {
    return false;
  }
  if (parsed.protocol === 'https:') return true;
  if (parsed.protocol === 'http:') return isLoopbackHostname(parsed.hostname);
  return false;
}
