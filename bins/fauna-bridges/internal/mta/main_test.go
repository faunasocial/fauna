package mta

import (
	"bytes"
	"os"
	"testing"

	faunaCore "github.com/faunasocial/fauna/libs/fauna-mail-go/fauna_core"

	"github.com/faunasocial/fauna/bins/fauna-bridges/internal/mailfauna"
)

// verifyUnparseableFixture is the one DATA payload the package's canned
// verifier answers with mailfauna.ErrAuthErrorUnparseable — a header line with
// no header/body separator, which mail-parser accepts, so Session.Data reaches
// the verify step. It is a fixture of this test double, not a model of what
// mail-auth refuses: mail-auth itself accepts it, which left the test that
// uses it skipping instead of asserting while it went through the real verifier.
const verifyUnparseableFixture = "From: alice@example.org"

// TestMain installs hermeticVerifyInbound as the package's verifier before any
// test runs. Set once, before m.Run, so parallel tests share it without a race.
func TestMain(m *testing.M) {
	verifyInbound = hermeticVerifyInbound
	os.Exit(m.Run())
}

// hermeticVerifyInbound stands in for mailfauna.VerifyInbound in every test of
// this package. The real one resolves SPF and DMARC over live DNS: a test's
// verdict then depended on whether the box could reach a resolver (convention
// 14, docs/goal/architecture/e2e-latency-independent-assertions.md), and on
// Windows its wildcard-bound UDP socket raised a firewall prompt on every run of
// the suite. The verdict mapping is pinned where it lives — the Rust pipeline's
// own hermetic test (libs/fauna-mail/tests/auth_tests.rs) — so this package pins
// only what Session.Data does with a verdict or a verify error.
func hermeticVerifyInbound(raw []byte, _, _, _ string) (mailfauna.AuthVerdicts, error) {
	if string(raw) == verifyUnparseableFixture {
		return mailfauna.AuthVerdicts{}, mailfauna.ErrAuthErrorUnparseable
	}
	var dmarc faunaCore.DmarcVerdict = faunaCore.DmarcVerdictNone{}
	switch {
	case bytes.Contains(raw, []byte(hermeticDmarcHeader+": pass\r\n")):
		dmarc = faunaCore.DmarcVerdictPass{}
	case bytes.Contains(raw, []byte(hermeticDmarcHeader+": fail\r\n")):
		// policy=none: the publisher observes only, so no enforce gate rejects
		// and the message is delivered with its DMARC fail on record.
		dmarc = faunaCore.DmarcVerdictFail{Policy: faunaCore.DmarcPolicyNone}
	}
	return mailfauna.AuthVerdicts{
		Dkim:  faunaCore.DkimVerdictNone{},
		Spf:   faunaCore.SpfVerdictNone,
		Dmarc: dmarc,
		Arc:   faunaCore.ArcVerdictNone,
	}, nil
}

// hermeticDmarcHeader is this test double's per-message DMARC knob: a DATA
// payload carrying `X-Hermetic-Dmarc: pass` (or `: fail`, CRLF-terminated) is
// answered with that DMARC verdict, every other payload with none. Keyed on the
// message bytes rather than a package variable so parallel tests each choose
// their own verdict without a race. The value in both arms is four bytes, so
// two otherwise-identical payloads differing only in the knob are the same
// length — the sealed-copy size assertions rely on that.
const hermeticDmarcHeader = "X-Hermetic-Dmarc"
