package tls

// FloorCertFetchDomain is the TLS-cert fetch key a bridge uses when the
// deployment has no primary domain — a domainless / bare-IP / localhost nest
// (the any-locator design). nest's fetch_tls_cert_blob seals whatever
// self-signed floor cert sits on its acme dir (read_acme_pem) regardless of the
// requested domain, so this string is only a fetch cache-key — it never reaches
// a client (the cert SANs do). "localhost" matches a floor-cert SAN and reads
// truthfully for a LAN/loopback deployment. Without this fallback a bridge would
// build no TLS provider when PrimaryDomain is empty and its listeners would have
// no cert to serve — the serving gap the any-locator track closes.
const FloorCertFetchDomain = "localhost"

// CertFetchDomain picks the domain key a bridge fetches its TLS cert under from
// nest's fauna.bridges.fetch_tls_cert_blob. With a primary domain it is that
// domain (nest seal-on-read seals the per-domain cert once provisioned); on a
// domainless / bare-IP / localhost nest (empty primaryDomain) it is the floor
// sentinel ([FloorCertFetchDomain]) so the bridge still builds a TLS provider and
// serves nest's self-signed floor cert. The string is an opaque fetch key — nest
// seals whatever cert is on disk regardless of it (the cert SANs are what a
// client validates against). Returning a non-empty value matters: [New] rejects
// an empty Domain.
//
// Shared by the mail bridge (MDA CalDAV/IMAP TLS) and the ATProto PDS bridge
// (`pds.<domain>` XRPC TLS) — both fetch the same on-disk cert keyed by the
// primary domain; the cert's SAN set (mail.<domain> / relay.<domain> /
// pds.<domain>) is what makes each subdomain's TLS validate.
func CertFetchDomain(primaryDomain string) string {
	if primaryDomain == "" {
		return FloorCertFetchDomain
	}
	return primaryDomain
}
