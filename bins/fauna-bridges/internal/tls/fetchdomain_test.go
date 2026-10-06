package tls

import "testing"

// CertFetchDomain returns the deployment's primary domain as the TLS-cert fetch
// key when one exists.
func TestCertFetchDomainPrimaryDomainWins(t *testing.T) {
	if got := CertFetchDomain("example.com"); got != "example.com" {
		t.Fatalf("a primary domain must be the fetch key, got %q", got)
	}
}

// A domainless nest (empty PrimaryDomain) falls back to the floor sentinel so
// the bridge still builds a TLS provider (New rejects an empty Domain).
func TestCertFetchDomainFallsBackToFloorWhenDomainless(t *testing.T) {
	got := CertFetchDomain("")
	if got != FloorCertFetchDomain {
		t.Fatalf("an empty primary domain must fall back to the floor sentinel %q, got %q", FloorCertFetchDomain, got)
	}
	if got == "" {
		t.Fatalf("the fetch key must be non-empty (New rejects an empty Domain)")
	}
}
