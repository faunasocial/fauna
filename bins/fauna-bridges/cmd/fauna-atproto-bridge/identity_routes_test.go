package main

import (
	"strings"
	"testing"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotofirehose"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotopds"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/atprotoread"
	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/xrpc"
)

// identityNSIDPrefix is the ATProto namespace whose procedures act on the DID
// document itself — the signed PLC operation, handle changes. The
// `atproto.commit` door's input is a record write and can never be one of these
// (`key-material-hierarchy.md` § *The oracle* → *The ATProto classes ride the
// protocol's own door*).
const identityNSIDPrefix = "com.atproto.identity."

// The deny list's structural half at the `atproto.commit` door: the PDS
// listener's whole XRPC table — every registrar main.go wires onto its one
// frame — serves no `com.atproto.identity.*` NSID, so a PLC operation is
// unreachable through this bridge. The headerless proxy default forwards
// `app.bsky.*` only (`atprotopds.proxyTarget`), so an unregistered identity
// NSID stays MethodNotImplemented. A registrar added to main.go joins this
// list in the same change.
func TestThePDSServesNoIdentityNSID(t *testing.T) {
	frame := xrpc.NewServer(nil, nil, nil, nil, nil)
	atprotopds.NewServer(nil, nil, nil, nil, nil).RegisterRoutes(frame)
	atprotoread.Register(frame, nil, nil)
	atprotofirehose.Register(frame, nil, nil)

	routes := frame.RegisteredRoutes()
	if len(routes) == 0 {
		t.Fatal("no routes registered — the table this pin reads is empty")
	}
	for nsid := range routes {
		if strings.HasPrefix(nsid, identityNSIDPrefix) {
			t.Errorf("%s is served — a DID-document operation is reachable on the PDS", nsid)
		}
	}
}
