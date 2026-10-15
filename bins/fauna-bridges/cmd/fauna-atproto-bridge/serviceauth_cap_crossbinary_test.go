package main

import (
	"testing"
	"time"

	faunaAtproto "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_bridge_atproto"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
)

// The service-auth lifetime cap has ONE home: shared Rust
// (`fauna_bridge_atproto::service_auth::MAX_LIFETIME_SECS`), which the nest's
// oracle door clamps an `exp` to before it asks this bridge to mint
// (key-material-hierarchy.md § The oracle → *The bridge-held custodian*).
// `internal/atprotopds` stays cgo-free by design, so it cannot read the
// constant over the binding; this pin holds its copy equal to the shared one,
// so the nest never promises a lifetime the bridge does not mint.
func TestServiceAuthCapIsTheSharedRustConstant(t *testing.T) {
	shared := time.Duration(faunaAtproto.ServiceAuthMaxLifetimeSecs()) * time.Second
	if atprotopds.MaxServiceAuthLifetime != shared {
		t.Fatalf("atprotopds.MaxServiceAuthLifetime = %v, shared Rust cap = %v — one constant, two readers",
			atprotopds.MaxServiceAuthLifetime, shared)
	}
}
