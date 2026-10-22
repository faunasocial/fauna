// Wire-level pins for the greeting re-check on a changed EHLO name
// (smtp-server.md § HELO / EHLO syntactic + identity validation), beside
// server_test.go's TestInboundBackendRegreetIsCheckedAgain (syntax and
// self-claim): the identity rule re-runs too, a re-EHLO mid-transaction drops
// the open MAIL (so the name DATA hands SPF/HELO evaluation is always the one
// checked at the MAIL before it — go-smtp's behaviour, measured here rather
// than read), and loopback keeps its exemption on a re-greet.
package mta

import (
	"net"
	"net/textproto"
	"testing"
	"time"
)

func regreetConn(t *testing.T, pol *Policy) *textproto.Conn {
	t.Helper()
	addr, cancel := startPolicyListener(t, pol)
	t.Cleanup(cancel)
	c, err := net.Dial("tcp", addr)
	if err != nil {
		t.Fatalf("dial: %v", err)
	}
	t.Cleanup(func() { _ = c.Close() })
	_ = c.SetDeadline(time.Now().Add(10 * time.Second))
	tc := textproto.NewConn(c)
	smtpExpect(t, tc, 220, "")
	return tc
}

// regreetPolicy: good.example resolves to the loopback test peer,
// bad.example elsewhere; test.example is a domain the listener hosts.
func regreetPolicy(identity bool) *Policy {
	res := newFakeResolver()
	res.host["good.example"] = []string{"127.0.0.1"}
	res.host["bad.example"] = []string{"5.6.7.8"}
	clk := newFakeClock(time.Date(2026, 10, 16, 12, 0, 0, 0, time.UTC))
	return &Policy{
		RateLimiter:          NewRateLimiter(60, time.Minute, clk),
		DNSBL:                NewDNSBLChecker(nil, res),
		FCrDNS:               NewFCrDNSChecker(res),
		FCrDNSMode:           FCrDNSModeOff,
		HELOIdentityRequired: identity,
		HELOResolver:         res,
		HELOLookupTimeout:    time.Second,
		SkipHELOLoopback:     true, // drive the non-loopback rules from 127.0.0.1
	}
}

func TestRegreetIdentityIsCheckedAgain(t *testing.T) {
	t.Parallel()
	tc := regreetConn(t, regreetPolicy(true))
	smtpExpect(t, tc, 250, "EHLO good.example")
	smtpExpect(t, tc, 250, "MAIL FROM:<>")
	smtpExpect(t, tc, 250, "RSET")
	smtpExpect(t, tc, 250, "EHLO bad.example")
	smtpExpect(t, tc, 554, "MAIL FROM:<>")
}

func TestRegreetMidTransactionDropsTheOpenMail(t *testing.T) {
	t.Parallel()
	tc := regreetConn(t, regreetPolicy(false))
	smtpExpect(t, tc, 250, "EHLO good.example")
	smtpExpect(t, tc, 250, "MAIL FROM:<>")
	smtpExpect(t, tc, 250, "EHLO test.example")
	// No RCPT (so no DATA) may ride on the MAIL checked under the old name.
	smtpExpect(t, tc, 502, "RCPT TO:<bob@test.example>")
	smtpExpect(t, tc, 554, "MAIL FROM:<>")
}

func TestRegreetLoopbackKeepsItsExemption(t *testing.T) {
	t.Parallel()
	pol := regreetPolicy(true)
	pol.SkipHELOLoopback = false
	tc := regreetConn(t, pol)
	smtpExpect(t, tc, 250, "EHLO good.example")
	smtpExpect(t, tc, 250, "MAIL FROM:<>")
	smtpExpect(t, tc, 250, "RSET")
	smtpExpect(t, tc, 250, "EHLO test.example")
	smtpExpect(t, tc, 250, "MAIL FROM:<>")
	smtpExpect(t, tc, 250, "RSET")
	smtpExpect(t, tc, 250, "EHLO bad.example")
	smtpExpect(t, tc, 250, "MAIL FROM:<>")
}
